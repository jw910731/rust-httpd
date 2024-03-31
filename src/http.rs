use std::{
    collections::HashMap,
    path::{self, Path},
    pin::Pin,
    str::FromStr,
    sync::Arc,
};

use anyhow::Result;
use include_dir::{include_dir, Dir};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter, EnumString, FromRepr};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::{self, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
};

#[derive(Clone, Copy, EnumIter, Debug, PartialEq, FromRepr, Hash, Eq, Display)]
pub enum Status {
    #[strum(to_string = "Continue")]
    Continue = 100,
    #[strum(to_string = "OK")]
    Ok = 200,
    #[strum(to_string = "Bad Request")]
    BadRequest = 400,
    #[strum(to_string = "Unauthorized")]
    Unauthorized = 401,
    #[strum(to_string = "Forbidden")]
    Forbidden = 403,
    #[strum(to_string = "Not Found")]
    NotFound = 404,
    #[strum(to_string = "Method Not Allowed")]
    MethodNotAllowed = 405,
    #[strum(to_string = "Payload Too Large")]
    PayloadTooLarge = 413, // Too big to fit in my mouth :o
    #[strum(to_string = "URI Too Long")]
    UriTooLong = 414,
    #[strum(to_string = "Internal Server Error")]
    InternalServerError = 500,
}

#[derive(Clone, Copy, EnumIter, Debug, PartialEq, EnumString)]
pub enum Method {
    GET,
    POST,
}

#[derive(Error, Debug)]
enum InternalError {
    #[error("format error")]
    FormatError,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("unknown error")]
    Unknown,
}

pub struct HttpHandleOption {
    pub status_page: HashMap<Status, Box<Path>>,
    pub serve_directory: Box<Path>,
}

pub struct HttpContext {
    cache_status_page: HashMap<Status, Arc<[u8]>>,
    serve_dir: Box<Path>,
}

static STATUS_PAGE_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/status_pages");

impl HttpContext {
    pub fn new(options: HttpHandleOption) -> Self {
        let mut cache = HashMap::<Status, Arc<[u8]>>::new();
        for status in Status::iter() {
            let content =
                if let Some(f) = STATUS_PAGE_DIR.get_file(format!("{}.html", status as u32)) {
                    Arc::from(f.contents())
                } else {
                    Arc::from(format!("<h1>{}<h1>", status as u32).as_bytes())
                };
            cache.insert(status, content);
        }
        HttpContext {
            cache_status_page: cache,
            serve_dir: options.serve_directory,
        }
    }
    pub fn get<R: AsyncRead, W: AsyncWrite>(self: Arc<Self>, rd: R, wr: W) -> HttpHandler<R, W> {
        HttpHandler {
            context: self,
            conn_rd: Box::pin(BufReader::new(rd)),
            conn_wr: Box::pin(BufWriter::new(wr)),
        }
    }
}

struct RequestHeader {
    method: Method,
    uri: String,
    user_agent: Option<String>,
    host: Option<String>,
}

struct ResponseHeader {
    status: Status,
    content_length: Option<usize>,
    content_type: String,
}

pub struct HttpHandler<R: AsyncRead, W: AsyncWrite> {
    context: Arc<HttpContext>,
    conn_rd: Pin<Box<BufReader<R>>>,
    conn_wr: Pin<Box<BufWriter<W>>>,
}

impl<R: AsyncRead, W: AsyncWrite> HttpHandler<R, W> {
    pub async fn handle(mut self) -> Result<()> {
        let req_header = self.request_header().await;
        match req_header {
            Err(e) => {
                let err_msg = e.to_string();
                let content = self
                    .context
                    .cache_status_page
                    .get(&Status::InternalServerError)
                    .unwrap()
                    .clone();
                self.response_header(ResponseHeader {
                    status: Status::InternalServerError,
                    content_length: Some(content.len() + err_msg.len()),
                    content_type: "text/plain".to_string(),
                })
                .await?;
                self.conn_wr.write_all(&content).await?;
                self.conn_wr.write_all(err_msg.as_bytes()).await?;
            }
            Ok(req) => {
                let path = self.context.serve_dir.join(req.uri);
                if path.starts_with(self.context.serve_dir.as_ref()) {
                    if let Ok(mut file) = fs::File::open(path).await {
                        if file.metadata().await.map(|metadata| metadata.is_file())? {
                            self.response_header(ResponseHeader {
                                status: Status::Ok,
                                content_length: file
                                    .metadata()
                                    .await
                                    .map(|metadata| metadata.len() as usize)
                                    .ok(),
                                content_type: "text/html".to_string(),
                            })
                            .await?;
                            self.body(&mut file).await?;
                            return Ok(());
                        }
                    }
                }
                let content = self
                    .context
                    .cache_status_page
                    .get(&Status::NotFound)
                    .unwrap()
                    .clone();
                self.response_header(ResponseHeader {
                    status: Status::Ok,
                    content_length: Some(content.len()),
                    content_type: "text/html".to_string(),
                })
                .await?;
                self.conn_wr.write_all(&content).await?;
            }
        }
        self.conn_wr.flush().await?;
        Ok(())
    }
    async fn request_header(&mut self) -> Result<RequestHeader> {
        let mut line = String::new();
        self.conn_rd.read_line(&mut line).await?;

        let (method, uri) = {
            let mut iter = line.split_ascii_whitespace();
            let method = Method::from_str(iter.next().ok_or(InternalError::MethodNotAllowed)?)
                .map_err(|e| InternalError::MethodNotAllowed)?;
            let uri = iter.next().ok_or(InternalError::FormatError)?.to_string();
            (method, uri)
        };
        line.clear();

        let mut host: Option<String> = None;
        let mut user_agent: Option<String> = None;
        // FIXME: check http version
        loop {
            self.conn_rd.read_line(&mut line).await?;
            if line == "\r\n" {
                break;
            }
            let mut iter = line.split(": ");
            match iter.next().ok_or(InternalError::FormatError)? {
                "Host" => {
                    host = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                "User-Agent" => {
                    user_agent = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                _ => {}
            }
            line.clear()
        }

        Ok(RequestHeader {
            method: method,
            uri: uri,
            host: host,
            user_agent: user_agent,
        })
    }
    async fn response_header(&mut self, resp: ResponseHeader) -> Result<()> {
        self.conn_wr
            .write(format!("HTTP/1.1 {} {}\r\n", resp.status as i32, resp.status).as_bytes())
            .await?;
        self.conn_wr.write(b"Server: Insomnia Server\r\n").await?;
        if let Some(sz) = resp.content_length {
            self.conn_wr
                .write(format!("Content-Length: {}\r\n", sz).as_bytes())
                .await?;
        }
        self.conn_wr
            .write(format!("Content-Type: {}\r\n", resp.content_type).as_bytes())
            .await?;
        self.conn_wr.write("\r\n".as_bytes()).await?;
        Ok(())
    }
    async fn body(&mut self, file: &mut File) -> Result<()> {
        io::copy(file, &mut self.conn_wr).await?;
        Ok(())
    }
}
