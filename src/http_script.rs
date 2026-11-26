//! The JavaScript of a `.http` file (`< {% … %}` before a request, `> {% … %}`
//! on its answer), run in Boa with the part of JetBrains' HTTP Client API
//! such scripts use: `client` (global, test, assert, log), `request`
//! (variables, environment, body), `response`, `crypto`. A script sees
//! nothing else: no files, no network. Never on the UI thread.

use std::collections::BTreeMap;

use boa_engine::{Context, JsResult, JsString, JsValue, NativeFunction, Source};

/// Loop turns a script may take: one that never ends is stopped.
const MOST_TURNS: u64 = 20_000_000;

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Test {
    pub name: String,
    pub ok: bool,
    #[serde(default)]
    pub message: String,
}

/// What a script left behind.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Ran {
    /// As the script left them: a number stays a number for the next script.
    pub globals: BTreeMap<String, serde_json::Value>,
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub tests: Vec<Test>,
    #[serde(default)]
    pub logs: Vec<String>,
    /// It threw, or could not be read.
    #[serde(skip)]
    pub error: Option<String>,
}

/// What a script is given. `response` is there for a handler only.
#[derive(serde::Serialize)]
pub struct Given<'a> {
    pub env: &'a BTreeMap<String, String>,
    pub globals: &'a BTreeMap<String, serde_json::Value>,
    pub variables: &'a BTreeMap<String, String>,
    pub request: Call<'a>,
    pub response: Option<Reply<'a>>,
}

#[derive(serde::Serialize)]
pub struct Call<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a str,
}

#[derive(serde::Serialize)]
pub struct Reply<'a> {
    pub status: u16,
    pub headers: &'a [(String, String)],
    pub body: &'a str,
    pub mime: &'a str,
    pub charset: &'a str,
}

const PRELUDE: &str = r#"
(function (input) {
  const out = { globals: input.globals, variables: input.variables, tests: [], logs: [] };
  const str = (v) => (typeof v === "string" ? v : JSON.stringify(v));
  const utf8Hex = (s) => {
    const e = encodeURIComponent(String(s));
    let h = "";
    for (let i = 0; i < e.length; i++) {
      if (e[i] === "%") { h += e.slice(i + 1, i + 3).toLowerCase(); i += 2; }
      else h += e.charCodeAt(i).toString(16).padStart(2, "0");
    }
    return h;
  };
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const hexToB64 = (hex) => {
    const bytes = [];
    for (let i = 0; i + 1 < hex.length; i += 2) bytes.push(parseInt(hex.slice(i, i + 2), 16));
    let s = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
      s += B64[(n >> 18) & 63] + B64[(n >> 12) & 63];
      s += i + 1 < bytes.length ? B64[(n >> 6) & 63] : "=";
      s += i + 2 < bytes.length ? B64[n & 63] : "=";
    }
    return s;
  };
  const b64ToHex = (b64) => {
    const clean = String(b64).replace(/-/g, "+").replace(/_/g, "/").replace(/[^A-Za-z0-9+/]/g, "");
    let bits = 0, n = 0, h = "";
    for (const c of clean) {
      n = (n << 6) | B64.indexOf(c);
      bits += 6;
      if (bits >= 8) { bits -= 8; h += ((n >> bits) & 255).toString(16).padStart(2, "0"); }
    }
    return h;
  };
  const hasher = (alg, key) => {
    let data = "";
    const h = {
      updateWithText(t) { data += utf8Hex(t); return h; },
      updateWithHex(x) { data += String(x).toLowerCase(); return h; },
      updateWithBase64(b) { data += b64ToHex(b); return h; },
      digest() {
        const hex = key === null ? __hash(alg, data) : __hmac(alg, key, data);
        return { toHex: () => hex, toBase64: () => hexToB64(hex) };
      },
    };
    return h;
  };
  const crypto = { hmac: {} };
  for (const alg of ["md5", "sha1", "sha256", "sha384", "sha512"]) {
    crypto[alg] = () => hasher(alg, null);
    crypto.hmac[alg] = () => ({
      withTextSecret: (s) => hasher(alg, utf8Hex(s)),
      withHexSecret: (s) => hasher(alg, String(s).toLowerCase()),
      withBase64Secret: (s) => hasher(alg, b64ToHex(s)),
    });
  }
  const plain = (value) => { const text = JSON.stringify(value); return text === undefined ? null : JSON.parse(text); };
  const store = (kept, typed) => ({
    set(name, value) { kept[name] = typed ? plain(value) : (value === undefined || value === null ? "" : str(value)); },
    get(name) { return Object.prototype.hasOwnProperty.call(kept, name) ? kept[name] : undefined; },
    isEmpty() { return Object.keys(kept).length === 0; },
    clear(name) { delete kept[name]; },
    clearAll() { for (const name of Object.keys(kept)) delete kept[name]; },
  });
  const headers = (list) => {
    const named = (name) => list.filter((h) => h[0].toLowerCase() === String(name).toLowerCase());
    const one = (h) => ({ name: h[0], value: () => h[1], getRawValue: () => h[1], tryGetSubstitutedValue: () => h[1] });
    return {
      valueOf(name) { const found = named(name); return found.length ? found[0][1] : null; },
      valuesOf(name) { return named(name).map((h) => h[1]); },
      findByName(name) { const found = named(name); return found.length ? one(found[0]) : null; },
      all() { return list.map(one); },
    };
  };
  const client = {
    global: store(out.globals, true),
    test(name, check) {
      try { check(); out.tests.push({ name: String(name), ok: true, message: "" }); }
      catch (e) { if (e && e.__exit) throw e; out.tests.push({ name: String(name), ok: false, message: String(e && e.message !== undefined ? e.message : e) }); }
    },
    assert(condition, message) { if (!condition) throw new Error(message || "Assertion failed"); },
    log(...said) { if (out.logs.length < 1000) out.logs.push(said.map((v) => (v === undefined ? "undefined" : str(v))).join(" ")); },
    exit() { throw { __exit: true }; },
  };
  const asked = (text) => Object.assign(() => text, { getRaw: () => text, tryGetSubstituted: () => text });
  const request = {
    variables: store(out.variables, false),
    environment: { get(name) { return Object.prototype.hasOwnProperty.call(input.env, name) ? input.env[name] : undefined; } },
    method: input.request.method,
    url: asked(input.request.url),
    body: asked(input.request.body),
    headers: headers(input.request.headers),
  };
  globalThis.client = client;
  globalThis.request = request;
  globalThis.crypto = crypto;
  globalThis.console = { log: client.log, info: client.log, warn: client.log, error: client.log };
  if (input.response) {
    const r = input.response;
    let body = r.body;
    try { if (/json/i.test(r.mime) || /^\s*[\[{]/.test(r.body)) body = JSON.parse(r.body); } catch (e) {}
    globalThis.response = { status: r.status, body, headers: headers(r.headers), contentType: { mimeType: r.mime, charset: r.charset } };
  }
  globalThis.__finish = () => JSON.stringify(out);
})
"#;

fn text(args: &[JsValue], ix: usize) -> String {
    args.get(ix).and_then(JsValue::as_string).map(|s| s.to_std_string_escaped()).unwrap_or_default()
}

fn from_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes().chunks_exact(2).filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()).collect()
}

fn to_hex(bytes: &[u8]) -> JsValue {
    JsValue::from(JsString::from(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().as_str()))
}

fn hash(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    use sha2::Digest as _;
    let data = from_hex(&text(args, 1));
    Ok(match text(args, 0).as_str() {
        "md5" => to_hex(&md5::Md5::digest(&data)),
        "sha1" => to_hex(&sha1::Sha1::digest(&data)),
        "sha384" => to_hex(&sha2::Sha384::digest(&data)),
        "sha512" => to_hex(&sha2::Sha512::digest(&data)),
        _ => to_hex(&sha2::Sha256::digest(&data)),
    })
}

fn hmac(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    use hmac::{Hmac, Mac as _};
    let (key, data) = (from_hex(&text(args, 1)), from_hex(&text(args, 2)));
    macro_rules! mac {
        ($hash:ty) => {{
            let mut mac = Hmac::<$hash>::new_from_slice(&key).expect("a key of any length");
            mac.update(&data);
            to_hex(&mac.finalize().into_bytes())
        }};
    }
    Ok(match text(args, 0).as_str() {
        "md5" => mac!(md5::Md5),
        "sha1" => mac!(sha1::Sha1),
        "sha384" => mac!(sha2::Sha384),
        "sha512" => mac!(sha2::Sha512),
        _ => mac!(sha2::Sha256),
    })
}

fn said(error: &boa_engine::JsError, context: &mut Context) -> String {
    match error.try_native(context) {
        Ok(native) => native.message().to_string(),
        Err(_) => error.to_string(),
    }
}

/// Runs `code` with what it is given; what it set and tested comes back
/// even when it threw.
pub fn run(code: &str, given: &Given) -> Ran {
    // The engine's parser is deep, and a worker's stack is small (512 KB on
    // macOS): on a thread of its own, with room.
    std::thread::scope(|scope| {
        let ran = std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(scope, || run_here(code, given));
        match ran.map(|thread| thread.join()) {
            Ok(Ok(ran)) => ran,
            _ => Ran { globals: given.globals.clone(), variables: given.variables.clone(), error: Some("the script could not be run".into()), ..Default::default() },
        }
    })
}

fn run_here(code: &str, given: &Given) -> Ran {
    let failed = |error: String| Ran { globals: given.globals.clone(), variables: given.variables.clone(), error: Some(error), ..Default::default() };
    let mut context = Context::default();
    context.runtime_limits_mut().set_loop_iteration_limit(MOST_TURNS);
    for (name, function) in [("__hash", hash as boa_engine::native_function::NativeFunctionPointer), ("__hmac", hmac)] {
        if let Err(e) = context.register_global_builtin_callable(JsString::from(name), 3, NativeFunction::from_fn_ptr(function)) {
            return failed(e.to_string());
        }
    }
    // JSON is JavaScript: the input goes in as a literal.
    let input = match serde_json::to_string(given) {
        Ok(input) => input,
        Err(e) => return failed(e.to_string()),
    };
    if let Err(e) = context.eval(Source::from_bytes(&format!("{PRELUDE}({input});"))) {
        return failed(said(&e, &mut context));
    }
    let error = context.eval(Source::from_bytes(code)).err().and_then(|e| {
        // client.exit() ends a script by throwing: not an error.
        let exit = e.as_opaque().and_then(JsValue::as_object).is_some_and(|thrown| thrown.has_property(JsString::from("__exit"), &mut context).unwrap_or(false));
        (!exit).then(|| said(&e, &mut context))
    });
    // What it left for later (a `then`, the rest of an async function).
    let _ = context.run_jobs();
    let finished = context.eval(Source::from_bytes("__finish()")).ok().and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()));
    let mut ran: Ran = match finished.as_deref().map(serde_json::from_str) {
        Some(Ok(ran)) => ran,
        _ => return failed(error.unwrap_or_else(|| "the script left nothing to read".into())),
    };
    ran.error = error;
    ran
}

#[cfg(test)]
mod tests {
    use super::*;

    fn given<'a>(env: &'a BTreeMap<String, String>, empty: &'a BTreeMap<String, String>, response: Option<Reply<'a>>) -> Given<'a> {
        static NONE: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        Given { env, globals: &NONE, variables: empty, request: Call { method: "POST", url: "http://x/y", headers: &[], body: "{\"email\":\"a@b.c\"}" }, response }
    }

    #[test]
    fn a_handler_tests_the_answer_and_keeps_what_the_next_request_needs() {
        let (env, empty) = (BTreeMap::new(), BTreeMap::new());
        let headers = [("Content-Type".to_string(), "application/json".to_string())];
        let reply = Reply { status: 200, headers: &headers, body: r#"{"clientId": 42, "token": "t-1", "list": [1, 2]}"#, mime: "application/json", charset: "utf-8" };
        let code = r#"
            client.test("registered", function () {
                client.assert(response.status === 200, "expected 200, got " + response.status);
            });
            client.test("has three", () => client.assert(response.body.list.length === 3, "only " + response.body.list.length));
            client.global.set("clientId", response.body.clientId);
            client.global.set("email", JSON.parse(request.body()).email);
            client.log("type", response.headers.valueOf("content-type"), response.contentType.mimeType);
        "#;
        let ran = run(code, &given(&env, &empty, Some(reply)));
        assert_eq!(ran.error, None);
        assert_eq!(ran.globals, BTreeMap::from([("clientId".to_string(), serde_json::json!(42)), ("email".to_string(), serde_json::json!("a@b.c"))]));
        assert_eq!(ran.tests, [Test { name: "registered".into(), ok: true, message: String::new() }, Test { name: "has three".into(), ok: false, message: "only 2".into() }]);
        assert_eq!(ran.logs, ["type application/json application/json"]);
    }

    #[test]
    fn a_script_before_a_request_signs_it() {
        let env = BTreeMap::from([("secret".to_string(), "key".to_string())]);
        let empty = BTreeMap::new();
        let code = r#"
            const sig = crypto.hmac.sha256().withTextSecret(request.environment.get("secret"))
                .updateWithText("The quick brown fox jumps over the lazy dog").digest();
            request.variables.set("sig", sig.toHex());
            request.variables.set("sig64", sig.toBase64());
            request.variables.set("sum", crypto.sha256().updateWithText("жук").digest().toHex());
            request.variables.set("md5", crypto.md5().updateWithText("").digest().toHex());
        "#;
        let ran = run(code, &given(&env, &empty, None));
        assert_eq!(ran.error, None);
        assert_eq!(ran.variables["sig"], "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
        assert_eq!(ran.variables["sig64"], "97yD9DBThCSxMpjmqm+xQ+9NWaFJRhdZl0edvC0aPNg=");
        use sha2::Digest as _;
        assert_eq!(from_hex(&ran.variables["sum"]), sha2::Sha256::digest("жук".as_bytes()).to_vec());
        assert_eq!(ran.variables["md5"], "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn a_script_that_throws_or_never_ends_says_so() {
        let (env, empty) = (BTreeMap::new(), BTreeMap::new());
        let ran = run("client.global.set('a', 1); client.assert(false, 'no');", &given(&env, &empty, None));
        assert_eq!((ran.error.as_deref(), ran.globals.get("a")), (Some("no"), Some(&serde_json::json!(1))));
        assert!(run("while (true) {}", &given(&env, &empty, None)).error.is_some());
        assert_eq!(run("client.log('a'); client.exit(); client.log('b');", &given(&env, &empty, None)).logs, ["a"]);
        assert!(run("let x = ;", &given(&env, &empty, None)).error.is_some());
    }
}
