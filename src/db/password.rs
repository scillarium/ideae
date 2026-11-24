//! A connection's password, in the system's keyring. Asked for only when a
//! connection is made or saved: the keyring may show its own dialog, which
//! nobody expects at the start of the app or from a page of Settings.
//!
//! On Linux an item is named after its connection (`ideae: Stage`), which the
//! window library's own credentials cannot do: it calls all of them
//! `zed-github-account`. One saved that way moves over when next read.

use anyhow::Result;
use gpui_kit::{App, Task};

/// Whether a connection has a password kept, as the form says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// In the keyring, under this name.
    As(String),
    Nothing,
    /// This system's keyring is not asked without need.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unknown,
    Unavailable(String),
}

fn url(id: &str) -> String {
    format!("{}://mysql/{id}", crate::state::APP_ID)
}

fn label(name: &str) -> String {
    format!("{}: {name}", crate::state::APP_ID)
}

/// Debug builds only: runs without a keyring (tests, the headless driver).
fn stand_in() -> Option<String> {
    if cfg!(debug_assertions) { std::env::var("IDEAE_DB_PASSWORD").ok() } else { None }
}

/// What the system calls where it keeps them.
pub fn keyring_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "the macOS Keychain"
    } else if cfg!(windows) {
        "Windows Credential Manager"
    } else {
        "the system keyring"
    }
}

#[cfg(target_os = "linux")]
mod service {
    use anyhow::{Context as _, Result};
    use oo7::Keyring;

    use super::{Kept, label, url};

    /// What the window library names every item it saves.
    const OLD_LABEL: &str = "zed-github-account";

    fn attributes(id: &str) -> [(&'static str, String); 3] {
        [("application", crate::state::APP_ID.to_string()), ("kind", "database-password".to_string()), ("connection", id.to_string())]
    }

    async fn old_item(keyring: &Keyring, id: &str) -> Result<Option<oo7::Item>> {
        for item in keyring.search_items(&[("url", url(id))]).await? {
            if item.label().await.is_ok_and(|l| l == OLD_LABEL) {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    pub async fn read(id: &str, name: &str) -> Result<Option<String>> {
        let keyring = Keyring::new().await?;
        keyring.unlock().await?;
        if let Some(item) = keyring.search_items(&attributes(id)).await?.into_iter().next() {
            item.unlock().await?;
            return Ok(Some(String::from_utf8_lossy(item.secret().await?.as_bytes()).into_owned()));
        }
        let Some(old) = old_item(&keyring, id).await? else { return Ok(None) };
        old.unlock().await?;
        let secret = old.secret().await?;
        let password = String::from_utf8_lossy(secret.as_bytes()).into_owned();
        keyring.create_item(&label(name), &attributes(id), password.as_str(), true).await?;
        // Gone only once the new one reads back the same.
        let moved = keyring.search_items(&attributes(id)).await?.into_iter().next().context("the moved password is not in the keyring")?;
        moved.unlock().await?;
        if moved.secret().await?.as_bytes() == secret.as_bytes() {
            old.delete().await?;
        }
        Ok(Some(password))
    }

    pub async fn write(id: &str, name: &str, password: &str) -> Result<()> {
        let keyring = Keyring::new().await?;
        keyring.unlock().await?;
        keyring.create_item(&label(name), &attributes(id), password, true).await?;
        if let Some(old) = old_item(&keyring, id).await? {
            old.delete().await?;
        }
        Ok(())
    }

    /// The connection was renamed: so is its item.
    pub async fn rename(id: &str, name: &str) -> Result<()> {
        let keyring = Keyring::new().await?;
        keyring.unlock().await?;
        for item in keyring.search_items(&attributes(id)).await? {
            item.set_label(&label(name)).await?;
        }
        Ok(())
    }

    pub async fn delete(id: &str) -> Result<()> {
        let keyring = Keyring::new().await?;
        keyring.unlock().await?;
        for item in keyring.search_items(&attributes(id)).await? {
            item.delete().await?;
        }
        if let Some(old) = old_item(&keyring, id).await? {
            old.delete().await?;
        }
        Ok(())
    }

    /// Looks without unlocking anything: a form opening asks nobody for a password.
    pub async fn kept(id: &str, name: &str) -> Kept {
        let found = async {
            let keyring = Keyring::new().await?;
            if let Some(item) = keyring.search_items(&attributes(id)).await?.into_iter().next() {
                return anyhow::Ok(Kept::As(item.label().await?));
            }
            Ok(if old_item(&keyring, id).await?.is_some() { Kept::As(label(name)) } else { Kept::Nothing })
        };
        found.await.unwrap_or_else(|e| Kept::Unavailable(e.to_string()))
    }
}

pub fn read(id: &str, name: &str, cx: &App) -> Task<Result<Option<String>>> {
    if let Some(password) = stand_in() {
        return Task::ready(Ok(Some(password)));
    }
    #[cfg(target_os = "linux")]
    {
        let (id, name) = (id.to_string(), name.to_string());
        cx.background_executor().spawn(async move { service::read(&id, &name).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        let found = cx.read_credentials(&url(id));
        cx.background_executor().spawn(async move {
            let found = found.await?;
            Ok(found.map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned()))
        })
    }
}

/// A connection's password to connect with: a production one only while unlocked.
pub fn read_for(connection: &super::config::Connection, cx: &App) -> Task<Result<Option<String>>> {
    if let Err(locked) = super::guard::may_run(connection) {
        return Task::ready(Err(anyhow::anyhow!(locked)));
    }
    read(&connection.id, &connection.name, cx)
}

pub fn write(id: &str, name: &str, password: &str, cx: &App) -> Task<Result<()>> {
    if stand_in().is_some() {
        return Task::ready(Ok(()));
    }
    #[cfg(target_os = "linux")]
    {
        let (id, name, password) = (id.to_string(), name.to_string(), password.to_string());
        cx.background_executor().spawn(async move { service::write(&id, &name, &password).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        cx.write_credentials(&url(id), "connection", password.as_bytes())
    }
}

/// After the connection's name changed, with its password left as it was.
pub fn rename(id: &str, name: &str, cx: &App) -> Task<Result<()>> {
    if stand_in().is_some() {
        return Task::ready(Ok(()));
    }
    #[cfg(target_os = "linux")]
    {
        let (id, name) = (id.to_string(), name.to_string());
        cx.background_executor().spawn(async move { service::rename(&id, &name).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (id, name, cx);
        Task::ready(Ok(()))
    }
}

pub fn delete(id: &str, cx: &App) -> Task<Result<()>> {
    if stand_in().is_some() {
        return Task::ready(Ok(()));
    }
    #[cfg(target_os = "linux")]
    {
        let id = id.to_string();
        cx.background_executor().spawn(async move { service::delete(&id).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        cx.delete_credentials(&url(id))
    }
}

/// For the form: whether there is one, and under what name.
pub fn kept(id: &str, name: &str, cx: &App) -> Task<Kept> {
    if stand_in().is_some() {
        return Task::ready(Kept::As(label(name)));
    }
    #[cfg(target_os = "linux")]
    {
        let (id, name) = (id.to_string(), name.to_string());
        cx.background_executor().spawn(async move { service::kept(&id, &name).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (id, name, cx);
        Task::ready(Kept::Unknown)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        future::Future,
        pin::pin,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
        thread::Thread,
    };

    use super::*;

    struct Unpark(Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    /// Only in a private keyring: `IDEAE_PRIVATE_KEYRING` is set by what started one.
    #[test]
    #[ignore]
    fn a_password_is_kept_under_its_connection_s_name() {
        if std::env::var_os("IDEAE_PRIVATE_KEYRING").is_none() {
            return eprintln!("no private keyring: skipped");
        }
        block_on(async {
            let id = "keyring-test";
            assert_eq!(service::kept(id, "Stage").await, Kept::Nothing);
            service::write(id, "Stage", "first").await.unwrap();
            assert_eq!(service::kept(id, "Stage").await, Kept::As(label("Stage")));
            assert_eq!(service::read(id, "Stage").await.unwrap().as_deref(), Some("first"));
            service::write(id, "Stage", "second").await.unwrap();
            assert_eq!(service::read(id, "Stage").await.unwrap().as_deref(), Some("second"), "replaced, not added");
            service::rename(id, "Production").await.unwrap();
            assert_eq!(service::kept(id, "Production").await, Kept::As(label("Production")));
            assert_eq!(service::read(id, "Production").await.unwrap().as_deref(), Some("second"));
            service::delete(id).await.unwrap();
            assert_eq!(service::kept(id, "Production").await, Kept::Nothing);

            // One saved the old way moves over when read, and only then is the old one gone.
            let keyring = oo7::Keyring::new().await.unwrap();
            keyring.unlock().await.unwrap();
            let old = "old-way";
            keyring.create_item("zed-github-account", &[("url", url(old)), ("username", "connection".to_string())], "kept", true).await.unwrap();
            assert_eq!(service::read(old, "Moved").await.unwrap().as_deref(), Some("kept"));
            assert_eq!(service::kept(old, "Moved").await, Kept::As(label("Moved")));
            assert!(keyring.search_items(&[("url", url(old))]).await.unwrap().is_empty(), "the old item is gone");
            assert_eq!(service::read(old, "Moved").await.unwrap().as_deref(), Some("kept"));
            service::delete(old).await.unwrap();
        });
    }
}
