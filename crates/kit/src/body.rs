//! Reading request bodies into params the way `Rack::Request#form_pairs` and
//! `ActionDispatch::Request#POST` do: JSON by content type, urlencoded forms (also for a POST
//! with no content type), and multipart with file parts spooled to temp files.

use std::io::Write;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, StatusCode, header};

use crate::format;
use crate::params::{self, ParamError, ParamMap, RawPair, UploadedFile};
use crate::request::media_type;

/// `Rack::Utils.multipart_total_part_limit` / `multipart_file_limit`.
pub const MULTIPART_PART_LIMIT: usize = 4096;
pub const MULTIPART_FILE_LIMIT: usize = 128;

/// The body as read: raw bytes (empty for multipart) and the params parsed from it.
#[derive(Debug, Clone)]
pub struct ParsedBody {
    pub raw: Bytes,
    pub params: Result<ParamMap, ParamError>,
}

impl ParsedBody {
    pub fn empty() -> Self {
        Self { raw: Bytes::new(), params: Ok(ParamMap::new()) }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error("request body too large")]
    TooLarge,
    #[error("error reading request body: {0}")]
    Read(String),
}

impl BodyError {
    pub fn status(&self) -> StatusCode {
        match self {
            BodyError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            BodyError::Read(_) => StatusCode::BAD_REQUEST,
        }
    }
}

/// Read and parse `body`. `original_method` is the method on the wire (Rack's `form_data?`
/// treats a content-type-less POST as a form).
pub async fn parse(
    original_method: &Method,
    headers: &HeaderMap,
    body: Body,
    limit: Option<usize>,
) -> Result<ParsedBody, BodyError> {
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).filter(|ct| !ct.is_empty());
    let media = media_type(content_type);

    if matches!(media.as_deref(), Some("multipart/form-data" | "multipart/related" | "multipart/mixed"))
        && let Some(boundary) = content_type.and_then(|ct| multer::parse_boundary(ct).ok()) {
            return parse_multipart(body, boundary, limit).await;
        }

    let raw = axum::body::to_bytes(body, limit.unwrap_or(usize::MAX)).await.map_err(|e| {
        if e.into_inner().downcast_ref::<http_body_util::LengthLimitError>().is_some() {
            BodyError::TooLarge
        } else {
            BodyError::Read("unreadable body".into())
        }
    })?;

    let is_json = format::content_mime_type(content_type).ok().flatten() == Some(&format::JSON);
    let params = if is_json && !raw.is_empty() {
        params::from_json_body(&raw)
    } else if media.as_deref() == Some("application/x-www-form-urlencoded")
        || (content_type.is_none() && *original_method == Method::POST)
        || matches!(media.as_deref(), Some("multipart/form-data" | "multipart/related" | "multipart/mixed"))
    {
        params::form_pairs(&raw).and_then(params::from_pairs)
    } else {
        Ok(ParamMap::new())
    };
    Ok(ParsedBody { raw, params })
}

async fn parse_multipart(body: Body, boundary: String, limit: Option<usize>) -> Result<ParsedBody, BodyError> {
    let mut constraints = multer::Constraints::new();
    if let Some(limit) = limit {
        constraints = constraints.size_limit(multer::SizeLimit::new().whole_stream(limit as u64));
    }
    let mut multipart = multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let mut pairs = Vec::new();
    let mut parts = 0;
    let mut files = 0;

    let result: Result<(), ParamError> = async {
        loop {
            let field = match multipart.next_field().await {
                Ok(Some(field)) => field,
                Ok(None) => break,
                Err(multer::Error::StreamSizeExceeded { .. }) => return Err(ParamError::Limit("too large".into())),
                Err(_) => return Err(ParamError::Parse),
            };
            parts += 1;
            if parts > MULTIPART_PART_LIMIT {
                return Err(ParamError::Limit("too many multipart parts".into()));
            }
            let part = Part::from_headers(field.headers());
            let mut field = field;

            match part.filename.as_deref() {
                // A blank filename means no file was selected: Rack drops the part.
                Some("") => while field.chunk().await.map_err(|_| ParamError::Parse)?.is_some() {},
                Some(filename) => {
                    files += 1;
                    if files > MULTIPART_FILE_LIMIT {
                        return Err(ParamError::Limit("too many files".into()));
                    }
                    let mut file = tempfile::Builder::new()
                        .prefix("RackMultipart")
                        .tempfile()
                        .map_err(|e| ParamError::Invalid(e.to_string()))?;
                    let mut size = 0u64;
                    while let Some(chunk) = field.chunk().await.map_err(|_| ParamError::Parse)? {
                        size += chunk.len() as u64;
                        file.write_all(&chunk).map_err(|e| ParamError::Invalid(e.to_string()))?;
                    }
                    let upload = UploadedFile::new(
                        filename.to_string(),
                        part.content_type.clone(),
                        part.head.clone(),
                        size,
                        file.into_temp_path(),
                    );
                    pairs.push(RawPair::file(&part.name(), upload));
                }
                None => {
                    let mut value = Vec::new();
                    while let Some(chunk) = field.chunk().await.map_err(|_| ParamError::Parse)? {
                        value.extend_from_slice(&chunk);
                    }
                    pairs.push(RawPair {
                        key: part.name().into_bytes(),
                        value: Some(params::PairValue::Bytes(value)),
                    });
                }
            }
        }
        Ok(())
    }
    .await;

    let params = match result {
        Err(ParamError::Limit(message)) if message == "too large" => return Err(BodyError::TooLarge),
        Err(error) => Err(error),
        Ok(()) => params::from_pairs(pairs),
    };
    Ok(ParsedBody { raw: Bytes::new(), params })
}

/// A multipart part's `Content-Disposition` and `Content-Type`, parsed like
/// `Rack::Multipart::Parser#consume_boundary`.
#[derive(Debug, Default, PartialEq)]
struct Part {
    name: Option<String>,
    filename: Option<String>,
    content_type: Option<String>,
    head: String,
}

impl Part {
    fn from_headers(headers: &HeaderMap) -> Self {
        let head: String = headers
            .iter()
            .map(|(k, v)| format!("{}: {}\r\n", k.as_str(), String::from_utf8_lossy(v.as_bytes())))
            .collect();
        let content_type = headers.get(header::CONTENT_TYPE).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
        let mut part = match headers.get(header::CONTENT_DISPOSITION) {
            Some(disposition) => parse_disposition(&String::from_utf8_lossy(disposition.as_bytes())),
            None => Part {
                name: headers.get("content-id").map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()),
                ..Part::default()
            },
        };
        part.content_type = content_type;
        part.head = head;
        part
    }

    /// Rack names nameless parts after the file, or `"<content type>[]"`.
    fn name(&self) -> String {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => self
                .filename
                .clone()
                .unwrap_or_else(|| format!("{}[]", self.content_type.as_deref().unwrap_or("text/plain"))),
        }
    }
}

fn parse_disposition(value: &str) -> Part {
    let mut part = Part::default();
    let mut filename_star = None;
    let mut rest = value.split_once(';').map(|(_, r)| r).unwrap_or("");
    while let Some(eq) = rest.find('=') {
        let param = rest[..eq].trim().to_ascii_lowercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some((_, '"')) => out.push('"'),
                        // IE sends unescaped Windows paths in filenames; keep the backslash.
                        Some((_, escaped)) if param == "filename" => {
                            out.push('\\');
                            out.push(escaped);
                        }
                        Some((_, escaped)) => out.push(escaped),
                        None => {}
                    },
                    c => out.push(c),
                }
            }
            value = out;
            rest = &quoted[end.min(quoted.len())..];
            rest = rest.split_once(';').map(|(_, r)| r).unwrap_or("");
        } else {
            let (v, r) = rest.split_once(';').unwrap_or((rest, ""));
            value = v.trim().to_string();
            rest = r;
        }
        match param.as_str() {
            "name" => part.name = Some(value),
            "filename" => part.filename = Some(value),
            "filename*" => filename_star = Some(value),
            _ => {}
        }
    }
    if let Some(star) = filename_star {
        let encoded = star.splitn(3, '\'').nth(2).unwrap_or("");
        part.filename = Some(normalize_filename(encoded));
    } else if let Some(filename) = part.filename.take() {
        part.filename = Some(normalize_filename(&filename));
    }
    part
}

/// `Rack::Multipart::Parser#normalize_filename`: unescape when every `%` is a valid escape, then
/// keep only the basename (browsers on Windows send full paths).
fn normalize_filename(filename: &str) -> String {
    let bytes = filename.as_bytes();
    let all_valid = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'%')
        .all(|(i, _)| bytes.get(i + 1..i + 3).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)));
    let unescaped = if all_valid {
        String::from_utf8_lossy(&percent_encoding::percent_decode(bytes).collect::<Vec<u8>>()).into_owned()
    } else {
        filename.to_string()
    };
    if unescaped.is_empty() {
        return unescaped;
    }
    unescaped.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispositions() {
        let part = parse_disposition(r#"form-data; name="message[attachment]"; filename="C:\Users\me\cat.png""#);
        assert_eq!(part.name.as_deref(), Some("message[attachment]"));
        assert_eq!(part.filename.as_deref(), Some("cat.png"));

        let part = parse_disposition(r#"form-data; name="a"; filename="with \"quotes\".txt""#);
        assert_eq!(part.filename.as_deref(), Some("with \"quotes\".txt"));

        let part = parse_disposition("form-data; name=file; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf");
        assert_eq!(part.name.as_deref(), Some("file"));
        assert_eq!(part.filename.as_deref(), Some("résumé.pdf"));

        let part = parse_disposition(r#"form-data; name="a"; filename="100%.txt""#);
        assert_eq!(part.filename.as_deref(), Some("100%.txt"));
    }

    fn multipart_body(boundary: &str, parts: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (disposition_and_headers, content) in parts {
            body.extend_from_slice(format!("--{boundary}\r\n{disposition_and_headers}\r\n\r\n{content}\r\n").as_bytes());
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    #[tokio::test]
    async fn multipart_fields_and_files() {
        let boundary = "XyZ";
        let body = multipart_body(
            boundary,
            &[
                (r#"Content-Disposition: form-data; name="_method""#, "patch"),
                (r#"Content-Disposition: form-data; name="user[name]""#, "Jo"),
                (
                    "Content-Disposition: form-data; name=\"user[avatar]\"; filename=\"me.png\"\r\nContent-Type: image/png",
                    "PNGDATA",
                ),
                (r#"Content-Disposition: form-data; name="user[empty]"; filename="""#, ""),
                (r#"Content-Disposition: form-data; name="tags[]""#, "a"),
                (r#"Content-Disposition: form-data; name="tags[]""#, "b"),
            ],
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}").parse().unwrap());
        let parsed = parse(&Method::POST, &headers, Body::from(body), None).await.unwrap();
        let params = parsed.params.unwrap();
        assert_eq!(params.str("_method"), Some("patch"));
        let user = params.get("user").unwrap();
        assert_eq!(user.get("name").and_then(|p| p.as_str()), Some("Jo"));
        assert!(user.get("empty").is_none());
        let avatar = user.get("avatar").and_then(|p| p.as_file()).unwrap();
        assert_eq!(avatar.original_filename, "me.png");
        assert_eq!(avatar.content_type.as_deref(), Some("image/png"));
        assert_eq!(avatar.size, 7);
        assert_eq!(avatar.read().unwrap(), b"PNGDATA");
        assert_eq!(params.to_json()["tags"], serde_json::json!(["a", "b"]));
    }

    #[tokio::test]
    async fn multipart_size_limit() {
        let body = multipart_body("B", &[(r#"Content-Disposition: form-data; name="f"; filename="x""#, &"x".repeat(1000))]);
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "multipart/form-data; boundary=B".parse().unwrap());
        let result = parse(&Method::POST, &headers, Body::from(body), Some(100)).await;
        assert!(matches!(result, Err(BodyError::TooLarge)));
    }

    #[tokio::test]
    async fn forms_json_and_raw_bodies() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/x-www-form-urlencoded".parse().unwrap());
        let parsed = parse(&Method::POST, &headers, Body::from("a[b]=1&c=2"), None).await.unwrap();
        assert_eq!(parsed.params.unwrap().to_json(), serde_json::json!({"a": {"b": "1"}, "c": "2"}));
        assert_eq!(parsed.raw.as_ref(), b"a[b]=1&c=2");

        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let parsed = parse(&Method::POST, &headers, Body::from(r#"{"url":"x"}"#), None).await.unwrap();
        assert_eq!(parsed.params.unwrap().to_json(), serde_json::json!({"url": "x"}));

        headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        let parsed = parse(&Method::POST, &headers, Body::from("Hello!"), None).await.unwrap();
        assert!(parsed.params.unwrap().is_empty());
        assert_eq!(parsed.raw.as_ref(), b"Hello!");

        // A POST without a content type is parsed as a form (Rack's form_data?).
        let parsed = parse(&Method::POST, &HeaderMap::new(), Body::from("Hello!"), None).await.unwrap();
        assert_eq!(parsed.params.unwrap().to_json(), serde_json::json!({"Hello!": null}));
        let parsed = parse(&Method::PUT, &HeaderMap::new(), Body::from("Hello!"), None).await.unwrap();
        assert!(parsed.params.unwrap().is_empty());

        let result = parse(&Method::POST, &HeaderMap::new(), Body::from("x".repeat(20)), Some(10)).await;
        assert!(matches!(result, Err(BodyError::TooLarge)));
    }
}
