use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const MAX_WS_FRAME_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB cap to prevent OOM on malformed frames

/// Lightweight, zero-dependency RFC 6455 loopback WebSocket client for Dart VM Service JSON-RPC 2.0.
pub struct VmServiceClient {
    stream: BufReader<TcpStream>,
    next_id: u64,
}

impl VmServiceClient {
    /// Connects to a Dart VM Service WebSocket URI (`ws://...` or `http://...`).
    /// Rejects `wss://` / `https://` explicitly since TLS is not supported on this loopback client.
    pub async fn connect(uri: &str) -> Result<Self, String> {
        let ws_uri = normalize_to_ws_uri(uri)?;
        let parsed =
            url::Url::parse(&ws_uri).map_err(|e| format!("Invalid VM Service URI: {e}"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| "VM Service URI missing host".to_string())?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| "VM Service URI missing port".to_string())?;
        let path_and_query = match parsed.query() {
            Some(q) => format!("{}?{q}", parsed.path()),
            None => parsed.path().to_string(),
        };

        let addr = format!("{host}:{port}");
        let mut stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&addr))
            .await
            .map_err(|_| format!("Timed out connecting to VM Service at {addr}"))?
            .map_err(|e| format!("Failed to connect to VM Service at {addr}: {e}"))?;

        let ws_key = encode_base64(&make_ws_nonce());
        let request = format!(
            "GET {path_and_query} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {ws_key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Failed to send WebSocket handshake: {e}"))?;

        let mut reader = BufReader::new(stream);

        // Wrap the ENTIRE HTTP upgrade response read in a single 3s timeout
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut status_line = String::new();
            reader
                .read_line(&mut status_line)
                .await
                .map_err(|e| format!("WebSocket handshake read error: {e}"))?;

            if !status_line.contains("101") {
                return Err(format!(
                    "VM Service rejected WebSocket upgrade: {}",
                    status_line.trim()
                ));
            }

            loop {
                let mut header = String::new();
                let n = reader
                    .read_line(&mut header)
                    .await
                    .map_err(|e| format!("Handshake header read error: {e}"))?;
                if n == 0 || header.trim().is_empty() {
                    break;
                }
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|_| "Timed out waiting for WebSocket handshake".to_string())??;

        Ok(Self {
            stream: reader,
            next_id: 1,
        })
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = json!({
            "jsonrpc": "2.0",
            "id": id.to_string(),
            "method": method,
            "params": params
        });
        let text = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        write_ws_frame(self.stream.get_mut(), 0x1, text.as_bytes()).await?;

        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("VM Service call `{method}` timed out"));
            }

            let frame_text =
                tokio::time::timeout(remaining, read_ws_text_frame(&mut self.stream))
                    .await
                    .map_err(|_| format!("VM Service call `{method}` timed out"))??;

            let Ok(msg) = serde_json::from_str::<Value>(&frame_text) else {
                continue;
            };

            let msg_id = msg.get("id").and_then(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            });

            if msg_id.as_deref() != Some(&id.to_string()) {
                continue;
            }

            if let Some(err) = msg.get("error") {
                let err_msg = err
                    .get("data")
                    .and_then(|d| d.get("details"))
                    .and_then(Value::as_str)
                    .or_else(|| err.get("message").and_then(Value::as_str))
                    .unwrap_or("VM Service returned an error");
                return Err(err_msg.to_string());
            }

            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub async fn get_main_isolate_id(&mut self) -> Result<String, String> {
        let vm = self.call("getVM", json!({})).await?;
        let isolates = vm
            .get("isolates")
            .and_then(Value::as_array)
            .ok_or_else(|| "VM Service returned no isolates".to_string())?;

        if let Some(main_iso) = isolates.iter().find(|iso| {
            iso.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.contains("main"))
        }) && let Some(id) = main_iso.get("id").and_then(Value::as_str)
        {
            return Ok(id.to_string());
        }

        isolates
            .first()
            .and_then(|iso| iso.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| "No active Dart isolate found".to_string())
    }

    pub async fn call_service_extension(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let isolate_id = self.get_main_isolate_id().await?;
        let mut obj = params.as_object().cloned().unwrap_or_default();
        obj.insert("isolateId".to_string(), json!(isolate_id));
        self.call(method, Value::Object(obj)).await
    }

    pub async fn capture_screenshot_png(&mut self) -> Result<Vec<u8>, String> {
        let result = self.call("_flutter.screenshot", json!({})).await?;
        let b64 = result
            .get("screenshot")
            .and_then(Value::as_str)
            .ok_or_else(|| "VM Service `_flutter.screenshot` returned no image data".to_string())?;
        decode_base64(b64)
    }
}

pub fn normalize_to_ws_uri(uri: &str) -> Result<String, String> {
    let trimmed = uri.trim();
    let mut parsed =
        url::Url::parse(trimmed).map_err(|e| format!("Invalid URI `{trimmed}`: {e}"))?;
    match parsed.scheme() {
        "ws" => Ok(parsed.to_string()),
        "http" => {
            let _ = parsed.set_scheme("ws");
            if !parsed.path().ends_with("/ws") {
                let path = parsed.path().trim_end_matches('/');
                parsed.set_path(&format!("{path}/ws"));
            }
            Ok(parsed.to_string())
        }
        "wss" | "https" => Err(format!(
            "TLS scheme `{}` is not supported by the local loopback VM Service client",
            parsed.scheme()
        )),
        other => Err(format!("Unsupported VM Service URI scheme `{other}`")),
    }
}

fn make_ws_nonce() -> [u8; 16] {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0x1234_5678_9abc_def0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) as u128;
    (nanos ^ (seq << 64)).to_le_bytes()
}

fn make_mask_key() -> [u8; 4] {
    static MASK_SEQ: AtomicU64 = AtomicU64::new(0x9e37_79b9);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5a5a_a5a5);
    let mixed = nanos ^ MASK_SEQ.fetch_add(0x7f4a_7c15, Ordering::Relaxed);
    (mixed as u32).to_le_bytes()
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if !bytes.len().is_multiple_of(4) {
        return Err("Invalid base64 length".to_string());
    }

    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.as_chunks::<4>().0 {
        let a = val(chunk[0]).ok_or_else(|| "Invalid base64 byte".to_string())?;
        let b = val(chunk[1]).ok_or_else(|| "Invalid base64 byte".to_string())?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            let c = val(chunk[2]).ok_or_else(|| "Invalid base64 byte".to_string())?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] != b'=' {
                let d = val(chunk[3]).ok_or_else(|| "Invalid base64 byte".to_string())?;
                out.push((c << 6) | d);
            }
        }
    }
    Ok(out)
}

async fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<(), String> {
    let mask_key = make_mask_key();
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | (opcode & 0x0f));

    let len = payload.len();
    if len < 126 {
        frame.push(0x80 | (len as u8));
    } else if len <= 65535 {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }

    frame.extend_from_slice(&mask_key);
    for (idx, &byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask_key[idx % 4]);
    }

    stream
        .write_all(&frame)
        .await
        .map_err(|e| format!("WebSocket write failed: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("WebSocket flush failed: {e}"))
}

async fn read_ws_text_frame(reader: &mut BufReader<TcpStream>) -> Result<String, String> {
    let mut assembled = Vec::new();

    loop {
        let mut header = [0u8; 2];
        reader
            .read_exact(&mut header)
            .await
            .map_err(|e| format!("WebSocket read header failed: {e}"))?;

        let fin = (header[0] & 0x80) != 0;
        let opcode = header[0] & 0x0f;
        let masked = (header[1] & 0x80) != 0;
        let mut payload_len = (header[1] & 0x7f) as u64;

        if payload_len == 126 {
            let mut ext = [0u8; 2];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|e| format!("WebSocket read len16 failed: {e}"))?;
            payload_len = u16::from_be_bytes(ext) as u64;
        } else if payload_len == 127 {
            let mut ext = [0u8; 8];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|e| format!("WebSocket read len64 failed: {e}"))?;
            payload_len = u64::from_be_bytes(ext);
        }

        if payload_len > MAX_WS_FRAME_BYTES {
            return Err(format!(
                "WebSocket frame size ({payload_len} bytes) exceeds 32 MiB safety limit"
            ));
        }

        let mask = if masked {
            let mut m = [0u8; 4];
            reader
                .read_exact(&mut m)
                .await
                .map_err(|e| format!("WebSocket read mask failed: {e}"))?;
            Some(m)
        } else {
            None
        };

        let mut data = vec![0u8; payload_len as usize];
        reader
            .read_exact(&mut data)
            .await
            .map_err(|e| format!("WebSocket read payload failed: {e}"))?;

        if let Some(m) = mask {
            for (i, b) in data.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }

        match opcode {
            0x8 => return Err("VM Service closed WebSocket connection".to_string()),
            0x9 => {
                // RFC 6455: Pong MUST echo the exact Ping payload with client masking
                let _ = write_ws_frame(reader.get_mut(), 0xa, &data).await;
                continue;
            }
            0xa => continue,
            0x1 | 0x0 => {
                if assembled.len() as u64 + data.len() as u64 > MAX_WS_FRAME_BYTES {
                    return Err("Assembled WebSocket message exceeds 32 MiB limit".to_string());
                }
                assembled.extend_from_slice(&data);
                if fin {
                    return String::from_utf8(assembled)
                        .map_err(|e| format!("Invalid UTF-8 in WebSocket frame: {e}"));
                }
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn normalizes_http_and_rejects_wss() {
        assert_eq!(
            normalize_to_ws_uri("http://localhost:50000/abc/").expect("ws"),
            "ws://localhost:50000/abc/ws"
        );
        assert!(normalize_to_ws_uri("wss://localhost:50000/abc/ws").is_err());
        assert!(normalize_to_ws_uri("https://localhost:50000/abc/").is_err());
    }

    #[tokio::test]
    async fn roundtrips_json_rpc_against_mock_vm_service_ws() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);

            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("read");
                if line.trim().is_empty() {
                    break;
                }
            }

            reader
                .get_mut()
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
                )
                .await
                .expect("101");

            for _ in 0..2 {
                let req_text = read_ws_text_frame(&mut reader).await.expect("read req");
                let req: Value = serde_json::from_str(&req_text).expect("json");
                let id = req["id"].as_str().expect("id");
                let method = req["method"].as_str().expect("method");

                let result = if method == "getVM" {
                    json!({
                        "isolates": [{ "id": "isolates/123", "name": "main" }]
                    })
                } else {
                    json!({ "type": "Success", "enabled": "true" })
                };

                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                })
                .to_string();

                let payload = resp.as_bytes();
                let mut frame = vec![0x81];
                if payload.len() < 126 {
                    frame.push(payload.len() as u8);
                } else {
                    frame.push(126);
                    frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                }
                frame.extend_from_slice(payload);
                reader.get_mut().write_all(&frame).await.expect("send resp");
            }
        });

        let mut client = VmServiceClient::connect(&format!("http://localhost:{port}/session/"))
            .await
            .expect("connect");
        let resp = client
            .call_service_extension("ext.flutter.debugPaint", json!({ "enabled": "true" }))
            .await
            .expect("call ext");
        assert_eq!(resp["enabled"], "true");

        server.await.expect("server done");
    }
}
