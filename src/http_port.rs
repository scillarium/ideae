//! Requests brought in and taken out of `.http` files: a curl command or a
//! Postman collection (v2) made a `.http` file's text, and a file's requests
//! made a Postman collection. Text in, text out: no files, no gpui.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::http_client::{BodyPart, Request};

/// A command line's words, as a shell would part them: quotes, `$'…'`,
/// backslashes, a line carried on with `\`.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') => {
                    chars.next_if_eq(&'\n');
                }
                Some(next) => {
                    word.push(next);
                    started = true;
                }
                None => {}
            },
            '\'' => {
                started = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                started = true;
                while let Some(q) = chars.next() {
                    match q {
                        '\'' => break,
                        '\\' => match chars.next() {
                            Some('n') => word.push('\n'),
                            Some('t') => word.push('\t'),
                            Some('r') => word.push('\r'),
                            Some(other) => word.push(other),
                            None => {}
                        },
                        q => word.push(q),
                    }
                }
            }
            '"' => {
                started = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => word.push(e),
                            Some('\n') => {}
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => {}
                        },
                        q => word.push(q),
                    }
                }
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The flags of curl that take a value and say nothing of the request.
const VALUED: &[&str] = &[
    "-o", "--output", "-m", "--max-time", "--connect-timeout", "-x", "--proxy", "--cacert", "--cert", "--key", "-w", "--write-out", "--retry", "-c", "--cookie-jar", "--resolve", "-T", "--upload-file",
    "--max-redirs", "--limit-rate", "-E", "--ciphers", "--interface", "-K", "--config",
];

/// A curl command as a request of a `.http` file.
pub fn from_curl(command: &str) -> Result<String, String> {
    let all = words(command.trim());
    let is_curl = |first: &String| {
        let name = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
        name == "curl" || name == "curl.exe"
    };
    if !all.first().is_some_and(is_curl) {
        return Err("Not a curl command: it begins with something else.".into());
    }
    // `-sSL` is `-s -S -L`, and `-XPOST` or `-H'A: b'` a flag with its value attached.
    const SHORT_VALUED: &str = "XHdFuAebomxwcTEK";
    let mut split = Vec::new();
    for word in all.into_iter().skip(1) {
        let short = word.starts_with('-') && !word.starts_with("--") && word.chars().count() > 2;
        if !short {
            split.push(word);
            continue;
        }
        let mut flags = word.char_indices().skip(1);
        while let Some((at, flag)) = flags.next() {
            split.push(format!("-{flag}"));
            if SHORT_VALUED.contains(flag) {
                if let Some(value) = word.get(at + flag.len_utf8()..).filter(|value| !value.is_empty()) {
                    split.push(value.to_string());
                }
                break;
            }
        }
    }
    let mut words = split.into_iter().peekable();
    let (mut method, mut url) = (None::<String>, None::<String>);
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let (mut query, mut json, mut no_redirect_said, mut form) = (false, false, true, Vec::new());
    while let Some(word) = words.next() {
        let (flag, joined) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value.to_string())),
            _ => (word.clone(), None),
        };
        let value = |words: &mut std::iter::Peekable<std::vec::IntoIter<String>>| joined.clone().or_else(|| words.next()).unwrap_or_default();
        match flag.as_str() {
            "-X" | "--request" => method = Some(value(&mut words).to_ascii_uppercase()),
            "-H" | "--header" => {
                let header = value(&mut words);
                if let Some((name, text)) = header.split_once(':') {
                    headers.push((name.trim().to_string(), text.trim().to_string()));
                }
            }
            "-d" | "--data" | "--data-binary" | "--data-ascii" => {
                let given = value(&mut words);
                // `@file`: the file's content, which a `.http` file writes as `< file`.
                match given.strip_prefix('@') {
                    Some(file) => files.push(file.to_string()),
                    None => data.push(given),
                }
            }
            "--data-raw" => data.push(value(&mut words)),
            "--data-urlencode" => {
                let given = value(&mut words);
                let encode = |text: &str| text.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
                data.push(match given.split_once('=') {
                    Some((name, text)) if !name.is_empty() => format!("{name}={}", encode(text)),
                    Some((_, text)) => encode(text),
                    None => encode(&given),
                });
            }
            "--json" => {
                json = true;
                data.push(value(&mut words));
            }
            "-F" | "--form" | "--form-string" => form.push(value(&mut words)),
            "-u" | "--user" => {
                use base64::Engine as _;
                let user = value(&mut words);
                let user = if user.contains(':') { user } else { format!("{user}:") };
                headers.push(("Authorization".into(), format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(user))));
            }
            "-A" | "--user-agent" => headers.push(("User-Agent".into(), value(&mut words))),
            "-e" | "--referer" => headers.push(("Referer".into(), value(&mut words))),
            "-b" | "--cookie" => headers.push(("Cookie".into(), value(&mut words))),
            "--url" => url = Some(value(&mut words)),
            "-I" | "--head" => method = Some("HEAD".into()),
            "-G" | "--get" => query = true,
            "-L" | "--location" => no_redirect_said = false,
            flag if VALUED.contains(&flag) => {
                value(&mut words);
            }
            flag if flag.starts_with("--retry") || flag.starts_with("--max-") || flag.ends_with("-timeout") || flag.ends_with("-time") => {
                value(&mut words);
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {}
            _ => url = url.or(Some(word)),
        }
    }
    let mut url = url.ok_or("The curl command names no URL.")?;
    let mut body = data.join("&");
    if query && !body.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&std::mem::take(&mut body));
    }
    let has = |headers: &[(String, String)], name: &str| headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
    if json && !has(&headers, "content-type") {
        headers.push(("Content-Type".into(), "application/json".into()));
        headers.push(("Accept".into(), "application/json".into()));
    }
    if !form.is_empty() {
        let boundary = "ideae-boundary";
        if !has(&headers, "content-type") {
            headers.push(("Content-Type".into(), format!("multipart/form-data; boundary={boundary}")));
        }
        for field in &form {
            let (name, value) = field.split_once('=').unwrap_or((field, ""));
            body.push_str(&format!("--{boundary}\n"));
            match value.strip_prefix('@') {
                Some(file) => {
                    let file = file.split(';').next().unwrap_or(file);
                    let shown = file.rsplit('/').next().unwrap_or(file);
                    body.push_str(&format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{shown}\"\n\n< {file}\n"));
                }
                None => body.push_str(&format!("Content-Disposition: form-data; name=\"{name}\"\n\n{value}\n")),
            }
        }
        body.push_str(&format!("--{boundary}--"));
    } else if (!body.is_empty() || !files.is_empty()) && !has(&headers, "content-type") {
        headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    }
    for file in &files {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("< {file}"));
    }
    let method = method.unwrap_or_else(|| if body.is_empty() { "GET".into() } else { "POST".into() });
    // curl does not follow a redirect unless told to, and this client does.
    let mut text = if no_redirect_said { "# @no-redirect\n".to_string() } else { String::new() };
    text.push_str(&format!("{method} {url}\n"));
    for (name, value) in &headers {
        text.push_str(&format!("{name}: {value}\n"));
    }
    if !body.is_empty() {
        text.push_str(&format!("\n{body}\n"));
    }
    Ok(text)
}

/// A name that is safe as a file's.
fn file_name(name: &str) -> String {
    let cleaned: String = name.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    // No dot in front: `..` would be the folder above.
    let cleaned = cleaned.trim_matches(['-', '.']).to_string();
    if cleaned.is_empty() { "requests".into() } else { cleaned }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The pairs of a Postman list of `{key, value}` that are not switched off.
fn pairs(list: Option<&Value>) -> Vec<(String, String)> {
    list.and_then(Value::as_array)
        .map(|list| list.iter().filter(|p| p.get("disabled").and_then(Value::as_bool) != Some(true)).map(|p| (field(p, "key").to_string(), field(p, "value").to_string())).collect())
        .unwrap_or_default()
}

fn postman_auth(auth: &Value) -> Option<(String, String)> {
    let of = |kind: &str, key: &str| pairs(auth.get(kind)).into_iter().find(|(k, _)| k == key).map(|(_, v)| v);
    match field(auth, "type") {
        "bearer" => Some(("Authorization".into(), format!("Bearer {}", of("bearer", "token")?))),
        "basic" => {
            use base64::Engine as _;
            let (user, password) = (of("basic", "username").unwrap_or_default(), of("basic", "password").unwrap_or_default());
            // With a variable in it, it cannot be encoded now: left for the reader.
            if user.contains("{{") || password.contains("{{") {
                return Some(("Authorization".into(), format!("Basic {user}:{password}")));
            }
            Some(("Authorization".into(), format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))))
        }
        "apikey" => Some((of("apikey", "key")?, of("apikey", "value").unwrap_or_default())),
        _ => None,
    }
}

fn postman_request(name: &str, item: &Value, inherited: Option<&Value>, out: &mut String) {
    let request = &item["request"];
    // A request may be only its URL.
    let (method, url) = match request {
        Value::String(url) => ("GET".to_string(), url.clone()),
        _ => {
            let url = match &request["url"] {
                Value::String(url) => url.clone(),
                url => field(url, "raw").to_string(),
            };
            (if field(request, "method").is_empty() { "GET".to_string() } else { field(request, "method").to_string() }, url)
        }
    };
    if url.is_empty() {
        return;
    }
    let mut headers = pairs(request.get("header"));
    let auth = request.get("auth").filter(|a| field(a, "type") != "inherit" && !a.is_null()).or(inherited);
    if let Some(header) = auth.and_then(postman_auth) {
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(&header.0)) {
            headers.push(header);
        }
    }
    let has = |headers: &[(String, String)], name: &str| headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
    let body = &request["body"];
    let text = match field(body, "mode") {
        "raw" => {
            if field(&body["options"]["raw"], "language") == "json" && !has(&headers, "content-type") {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            field(body, "raw").to_string()
        }
        "urlencoded" => {
            if !has(&headers, "content-type") {
                headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
            }
            pairs(body.get("urlencoded")).iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
        }
        "graphql" => {
            if !has(&headers, "content-type") {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            let variables = serde_json::from_str::<Value>(field(&body["graphql"], "variables")).unwrap_or(json!({}));
            serde_json::to_string_pretty(&json!({ "query": field(&body["graphql"], "query"), "variables": variables })).unwrap_or_default()
        }
        "formdata" => {
            let boundary = "ideae-boundary";
            if !has(&headers, "content-type") {
                headers.push(("Content-Type".into(), format!("multipart/form-data; boundary={boundary}")));
            }
            let mut text = String::new();
            for part in body["formdata"].as_array().into_iter().flatten().filter(|p| p.get("disabled").and_then(Value::as_bool) != Some(true)) {
                text.push_str(&format!("--{boundary}\n"));
                match field(part, "type") {
                    "file" => {
                        let file = field(part, "src");
                        text.push_str(&format!("Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\n\n< {file}\n", field(part, "key"), file.rsplit('/').next().unwrap_or(file)));
                    }
                    _ => text.push_str(&format!("Content-Disposition: form-data; name=\"{}\"\n\n{}\n", field(part, "key"), field(part, "value"))),
                }
            }
            text.push_str(&format!("--{boundary}--"));
            text
        }
        _ => String::new(),
    };
    out.push_str(&format!("### {}\n", name.replace('\n', " ")));
    let scripted = item["event"].as_array().is_some_and(|events| events.iter().any(|e| e["script"]["exec"].as_array().is_some_and(|lines| lines.iter().any(|l| !l.as_str().unwrap_or_default().trim().is_empty()))));
    if scripted {
        out.push_str("# Postman's scripts (pm.*) were not brought over: write them again as > {% client.test(…) %}\n");
    }
    // On one line each: a line break in a URL or a header would be read as
    // the request's next part, a script or a file among them.
    let one_line = |text: &str| text.split(['\r', '\n']).map(str::trim).collect::<Vec<_>>().join("");
    out.push_str(&format!("{} {}\n", one_line(&method), one_line(&url)));
    for (name, value) in &headers {
        out.push_str(&format!("{}: {}\n", one_line(name), one_line(value)));
    }
    if !text.is_empty() {
        // A body's line that would be read as the file's own (`###`, `< file`, a script) is set in by a space.
        let body: Vec<String> = text.trim_end().lines().map(|line| if line.starts_with("###") || line.starts_with("< ") || line.starts_with('>') { format!(" {line}") } else { line.to_string() }).collect();
        out.push_str(&format!("\n{}\n", body.join("\n").replace("{{$guid}}", "{{$uuid}}")));
    }
    out.push('\n');
}

fn postman_items(items: &Value, prefix: &str, auth: Option<&Value>, out: &mut String) {
    for item in items.as_array().into_iter().flatten() {
        let name = if prefix.is_empty() { field(item, "name").to_string() } else { format!("{prefix} / {}", field(item, "name")) };
        let auth = item.get("auth").filter(|a| !a.is_null() && field(a, "type") != "inherit").or(auth);
        if item.get("item").is_some() {
            postman_items(&item["item"], &name, auth, out);
        } else if item.get("request").is_some() {
            postman_request(&name, item, auth, out);
        }
    }
}

/// A Postman collection (v2.0 or v2.1) as `.http` files, by name: one for
/// each of its top folders, and one for the requests outside them.
pub fn from_postman(text: &str) -> Result<Vec<(String, String)>, String> {
    let collection: Value = serde_json::from_str(text).map_err(|e| format!("Not JSON: {e}"))?;
    if !collection["info"]["schema"].as_str().is_some_and(|schema| schema.contains("/collection/v2")) {
        return Err("Not a Postman collection of version 2: export it from Postman as Collection v2.1.".into());
    }
    let title = file_name(field(&collection["info"], "name"));
    let auth = collection.get("auth").filter(|a| !a.is_null());
    let mut variables = String::new();
    for (key, value) in pairs(collection.get("variable")) {
        variables.push_str(&format!("@{key} = {value}\n"));
    }
    if !variables.is_empty() {
        variables.push('\n');
    }
    let mut files: Vec<(String, String)> = Vec::new();
    let mut loose = String::new();
    for item in collection["item"].as_array().into_iter().flatten() {
        let auth = item.get("auth").filter(|a| !a.is_null() && field(a, "type") != "inherit").or(auth);
        if item.get("item").is_some() {
            let mut out = String::new();
            postman_items(&item["item"], "", auth, &mut out);
            if !out.is_empty() {
                files.push((format!("{title}/{}.http", file_name(field(item, "name"))), format!("{variables}{out}")));
            }
        } else if item.get("request").is_some() {
            postman_request(field(item, "name"), item, auth, &mut loose);
        }
    }
    if !loose.is_empty() {
        files.insert(0, (if files.is_empty() { format!("{title}.http") } else { format!("{title}/{title}.http") }, format!("{variables}{loose}")));
    }
    if files.is_empty() {
        return Err("The collection has no requests.".into());
    }
    // Two folders of one name would write over each other.
    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &mut files {
        let stem = name.trim_end_matches(".http").to_string();
        let mut count = 1;
        while !seen.insert(name.to_lowercase()) {
            count += 1;
            *name = format!("{stem}-{count}.http");
        }
    }
    Ok(files)
}

/// A file's requests as a Postman collection (v2.1). Its scripts stay
/// behind: Postman's are another API.
pub fn to_postman(name: &str, requests: &[Request], file_vars: &BTreeMap<String, String>) -> String {
    let items: Vec<Value> = requests
        .iter()
        .map(|request| {
            let headers: Vec<Value> = request.headers.iter().map(|(key, value)| json!({ "key": key, "value": value })).collect();
            let body: String = request
                .body
                .iter()
                .map(|part| match part {
                    BodyPart::Text(text) => text.clone(),
                    BodyPart::File(file) => format!("< {file}"),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let json_body = request.headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("content-type") && v.contains("json"));
            let mut made = json!({ "method": request.method, "header": headers, "url": { "raw": request.url } });
            if !body.is_empty() {
                made["body"] = json!({ "mode": "raw", "raw": body, "options": { "raw": { "language": if json_body { "json" } else { "text" } } } });
            }
            json!({ "name": request.title(), "request": made })
        })
        .collect();
    let variables: Vec<Value> = file_vars.iter().map(|(key, value)| json!({ "key": key, "value": value })).collect();
    let collection = json!({
        "info": { "name": name, "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
        "item": items,
        "variable": variables,
    });
    serde_json::to_string_pretty(&collection).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::parse;

    #[test]
    fn a_curl_command_is_a_request() {
        let text = from_curl(
            "curl -L -X POST 'https://example.com/api/users?x=1' \\\n  -H 'Content-Type: application/json' \\\n  -H \"Authorization: Bearer a'b\" \\\n  --data-raw '{\"name\": \"Ann O'\\''Neil\"}' --compressed -o out.txt",
        )
        .unwrap();
        assert_eq!(text, "POST https://example.com/api/users?x=1\nContent-Type: application/json\nAuthorization: Bearer a'b\n\n{\"name\": \"Ann O'Neil\"}\n");
        let (requests, _) = parse(&text);
        assert_eq!((requests.len(), requests[0].headers.len()), (1, 2));

        let get = from_curl("curl example.com -G -d a=1 --data-urlencode=b=2 -u me:secret").unwrap();
        assert_eq!(get, "# @no-redirect\nGET example.com?a=1&b=2\nAuthorization: Basic bWU6c2VjcmV0\n");
        assert_eq!(from_curl("curl --json '{}' http://x").unwrap(), "# @no-redirect\nPOST http://x\nContent-Type: application/json\nAccept: application/json\n\n{}\n");
        assert_eq!(from_curl("curl http://x -d $'a\\nb'").unwrap(), "# @no-redirect\nPOST http://x\nContent-Type: application/x-www-form-urlencoded\n\na\nb\n");
        assert_eq!(from_curl("curl.exe -sSLo out.json -XPUT -H'A: b' --retry-delay 2 http://x --data-binary @payload.json").unwrap(), "PUT http://x\nA: b\nContent-Type: application/x-www-form-urlencoded\n\n< payload.json\n");
        assert_eq!(from_curl("curl -G http://x --data-urlencode 'q=hello world&x' -u me").unwrap(), "# @no-redirect\nGET http://x?q=hello%20world%26x\nAuthorization: Basic bWU6\n");
        assert!(from_curl("wget http://x").is_err());
        assert!(from_curl("curl -s").is_err());
    }

    #[test]
    fn a_postman_collection_comes_in_and_a_file_goes_out() {
        let collection = r#"{
            "info": {"name": "Core API", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
            "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}"}]},
            "variable": [{"key": "host", "value": "https://example.com"}],
            "item": [
                {"name": "Ping", "request": {"method": "GET", "url": {"raw": "{{host}}/ping"}, "header": [{"key": "X-Off", "value": "1", "disabled": true}]}},
                {"name": "Users", "item": [
                    {"name": "Create", "event": [{"listen": "test", "script": {"exec": ["pm.test('x')"]}}],
                     "request": {"method": "POST", "url": "{{host}}/users", "header": [{"key": "Accept", "value": "application/json"}],
                                 "body": {"mode": "raw", "raw": "{\"a\": 1}", "options": {"raw": {"language": "json"}}}}},
                    {"name": "Deep", "item": [{"name": "Form", "auth": {"type": "noauth"}, "request": {"method": "PUT", "url": "{{host}}/f",
                        "body": {"mode": "urlencoded", "urlencoded": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}]}}}]}
                ]}
            ]
        }"#;
        let files = from_postman(collection).unwrap();
        assert_eq!(files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["Core-API/Core-API.http", "Core-API/Users.http"]);
        assert_eq!(files[0].1, "@host = https://example.com\n\n### Ping\nGET {{host}}/ping\nAuthorization: Bearer {{token}}\n\n");
        let (users, vars) = parse(&files[1].1);
        assert_eq!(vars["host"], "https://example.com");
        assert_eq!(users.iter().map(|r| (r.title(), r.method.clone())).collect::<Vec<_>>(), [("Create".to_string(), "POST".to_string()), ("Deep / Form".to_string(), "PUT".to_string())]);
        assert_eq!(users[0].headers.len(), 3, "{:?}", users[0].headers);
        assert_eq!(users[1].body, [BodyPart::Text("a=1&b=2".into())]);
        assert_eq!(users[1].headers, [("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())]);
        assert!(from_postman("{}").is_err());
        // Names that would leave the folder or write over each other, and a URL with a line break in it.
        let sly = r#"{"info": {"name": "..", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}, "item": [
            {"name": "A", "item": [{"name": "x", "request": {"method": "GET", "url": "http://x/a\n\n> {% client.log(1) %}"}}]},
            {"name": "A", "item": [{"name": "y", "request": {"method": "POST", "url": "http://x/b", "body": {"mode": "raw", "raw": "a\n### b\n< c"}}}]},
            {"name": "A-2", "item": [{"name": "z", "request": "http://x/c"}]}]}"#;
        let files = from_postman(sly).unwrap();
        assert_eq!(files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["requests/A.http", "requests/A-2.http", "requests/A-2-2.http"]);
        let (first, _) = parse(&files[0].1);
        assert_eq!((first.len(), first[0].url.as_str(), first[0].post_script.as_str()), (1, "http://x/a> {% client.log(1) %}", ""));
        let (second, _) = parse(&files[1].1);
        assert_eq!((second.len(), second[0].body.clone()), (1, vec![BodyPart::Text("a\n ### b\n < c".into())]));

        // And back: what went out comes in the same.
        let back = from_postman(&to_postman("Users", &users, &vars)).unwrap();
        let (again, vars_again) = parse(&back[0].1);
        assert_eq!(back[0].0, "Users.http");
        assert_eq!(vars_again, vars);
        assert_eq!(again.iter().map(|r| (r.title(), r.url.clone(), r.body.clone())).collect::<Vec<_>>(), users.iter().map(|r| (r.title(), r.url.clone(), r.body.clone())).collect::<Vec<_>>());
    }
}
