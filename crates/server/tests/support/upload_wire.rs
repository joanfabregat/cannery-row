//! Task-owned S3-compatible peer exercising the reviewed official SDK.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Write,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub struct WireState {
    pub mode: String,
    pub calls: Vec<Value>,
}
pub struct WireTask(tokio::task::JoinHandle<()>);
impl Drop for WireTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub async fn start() -> Result<(String, Arc<Mutex<WireState>>, WireTask)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let state = Arc::new(Mutex::new(WireState {
        mode: "single".to_owned(),
        calls: vec![],
    }));
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut children = JoinSet::new();
        loop {
            tokio::select! {
                incoming=listener.accept()=>{let Ok((socket,_))=incoming else {break;};let shared=shared.clone();children.spawn(async move {let _=serve(socket,shared).await;});},
                _=children.join_next(),if !children.is_empty()=>{},
            }
        }
    });
    Ok((endpoint, state, WireTask(task)))
}
#[allow(
    clippy::too_many_lines,
    reason = "Bounded S3 protocol fixture response recipes"
)]
async fn serve(mut socket: tokio::net::TcpStream, state: Arc<Mutex<WireState>>) -> Result<()> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    let boundary = loop {
        let size = socket.read(&mut buf).await?;
        if size == 0 {
            return Err("SDK request ended early".into());
        }
        bytes.extend_from_slice(&buf[..size]);
        if let Some(position) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() > 65536 {
            return Err("bounded SDK request headers".into());
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next().ok_or("request line")?.split_whitespace();
    let method = request.next().ok_or("method")?.to_owned();
    let path = request.next().ok_or("path")?.to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(a, b)| (a.to_ascii_lowercase(), b.trim().to_owned()))
        .collect();
    let size = headers
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if size > 9 * (1 << 20) {
        return Err("bounded SDK control body".into());
    }
    while bytes.len() < boundary + size {
        let length = socket.read(&mut buf).await?;
        if length == 0 {
            return Err("SDK body ended early".into());
        }
        bytes.extend_from_slice(&buf[..length]);
    }
    let body = &bytes[boundary..boundary + size];
    let mode = {
        let mut state = state.lock().map_err(|_| "wire tracking")?;
        let body = if state.mode == "relay" && body.len() > 65536 {
            assert_eq!(body.len(), 8 * (1 << 20));
            assert!(body.iter().all(|byte| *byte == b'a'));
            format!("sha256:{:x}", Sha256::digest(body))
        } else {
            super::hex(body)
        };
        state.calls.push(json!({"method":method,"path":path,"checksum_mode":headers.get("x-amz-checksum-mode"),"content_type":headers.get("content-type"),"body":body}));
        state.mode.clone()
    };
    let mut code = 200;
    let mut payload = Vec::new();
    let mut extra = String::new();
    let mut declared = None;
    if method == "HEAD" {
        if mode == "missing" || mode == "multipart-gone-missing" || mode == "relay" {
            code = 404;
        } else if mode == "head-error" {
            code = 500;
        } else {
            let data = if mode.starts_with("multipart") {
                vec![b'a'; 6 * (1 << 20)]
            } else {
                b"hello".to_vec()
            };
            declared = Some(data.len() + usize::from(mode == "wrong-size"));
            extra.push_str("etag: \"wire-generation\"\r\n");
            if mode != "no-checksum" {
                use base64::Engine;
                let digest = Sha256::digest(if mode == "wrong-sha" || mode == "delete-error" {
                    b"wrong".as_slice()
                } else {
                    &data
                });
                write!(
                    extra,
                    "x-amz-checksum-sha256: {}\r\n",
                    base64::engine::general_purpose::STANDARD.encode(digest)
                )?;
            }
        }
    } else if method == "GET" {
        if path.contains("uploadId=") {
            if mode.starts_with("multipart-gone") {
                code = 404;
                payload = b"<Error><Code>NoSuchUpload</Code></Error>".to_vec();
            } else {
                let parts = if mode == "multipart-missing-parts" {
                    ""
                } else {
                    "<Part><PartNumber>1</PartNumber><ETag>\"one\"</ETag><Size>5242880</Size></Part><Part><PartNumber>2</PartNumber><ETag>\"two\"</ETag><Size>1048576</Size></Part>"
                };
                payload = format!(
                    "<ListPartsResult><IsTruncated>false</IsTruncated>{parts}</ListPartsResult>"
                )
                .into_bytes();
            }
        } else {
            payload = if mode.starts_with("multipart") {
                vec![b'a'; 6 * (1 << 20)]
            } else {
                b"hello".to_vec()
            };
        }
    } else if method == "POST" {
        if path.contains("uploads") {
            if mode == "create-error" {
                code = 500;
                payload = b"<Error><Code>InternalError</Code></Error>".to_vec();
            } else {
                payload=b"<InitiateMultipartUploadResult><Bucket>fixture</Bucket><UploadId>fixture-upload</UploadId></InitiateMultipartUploadResult>".to_vec();
            }
        } else if mode == "multipart-rejected" {
            code = 400;
            payload = b"<Error><Code>InvalidPart</Code></Error>".to_vec();
        } else {
            payload=b"<CompleteMultipartUploadResult><ETag>\"wire-generation\"</ETag></CompleteMultipartUploadResult>".to_vec();
        }
    } else if method == "PUT" {
        assert_eq!(mode, "relay");
        extra.push_str("etag: \"relay-part\"\r\n");
    } else if method == "DELETE" {
        if mode == "delete-error" {
            code = 500;
            payload = b"<Error><Code>InternalError</Code></Error>".to_vec();
        } else {
            code = 204;
        }
    }
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let reply = format!(
        "HTTP/1.1 {code} {reason}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n",
        declared.unwrap_or(payload.len())
    );
    socket.write_all(reply.as_bytes()).await?;
    if method != "HEAD" {
        socket.write_all(&payload).await?;
    }
    socket.shutdown().await?;
    Ok(())
}
