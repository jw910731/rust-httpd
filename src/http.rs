use std::{collections::HashMap, io::ErrorKind, path::Path, pin::Pin, str::FromStr, sync::Arc};

use anyhow::{Error, Result};
use include_dir::{include_dir, Dir};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter, EnumString, FromRepr};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::{
        self, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
        BufWriter,
    },
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

#[derive(Clone, Copy, EnumIter, Debug, PartialEq, EnumString, Display)]
pub enum Method {
    GET,
    POST,
}

#[derive(Error, Debug, PartialEq)]
enum InternalError {
    #[error("format error")]
    FormatError,
    #[error("required header field is not provided")]
    RequiredHeaderField,
    #[error("eof reached without expectation")]
    EOFReached,
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

#[derive(Clone, Copy, Debug, PartialEq, EnumString)]
pub enum ConnectionType {
    #[strum(serialize = "keep-alive")]
    KeepAlive,
    #[strum(serialize = "close")]
    Close,
}

struct RequestHeader {
    method: Method,
    uri: String,
    user_agent: Option<String>,
    host: Option<String>,
    content_length: Option<usize>,
    connection: ConnectionType,
}

impl RequestHeader {
    fn parse(raw: &str) -> Result<Self> {
        let mut line_iter = raw.split("\r\n");
        let mut iter = line_iter
            .next()
            .ok_or(InternalError::FormatError)?
            .split_ascii_whitespace();
        let method = Method::from_str(iter.next().ok_or(InternalError::FormatError)?)
            .map_err(|_| InternalError::FormatError)?;
        let uri = iter.next().ok_or(InternalError::FormatError)?.to_string();

        let mut host: Option<String> = None;
        let mut user_agent: Option<String> = None;
        let mut conetnt_length: Option<usize> = None;
        let mut connection = ConnectionType::KeepAlive;
        // FIXME: check http version
        for line in line_iter {
            let mut iter = line.split(": ").map(|sec| sec.replace("\r\n", ""));
            match iter.next().ok_or(InternalError::FormatError)?.as_str() {
                "Host" => {
                    host = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                "User-Agent" => {
                    user_agent = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                "Content-Length" => {
                    conetnt_length = Some(
                        iter.next()
                            .map(|sz| sz.parse::<usize>().ok())
                            .flatten()
                            .ok_or(InternalError::FormatError)?,
                    )
                }
                "Connection" => {
                    connection = ConnectionType::from_str(
                        iter.next().ok_or(InternalError::FormatError)?.as_str(),
                    )?
                }
                _ => {}
            }
        }
        Ok(RequestHeader {
            method: method,
            uri: uri,
            host: host,
            user_agent: user_agent,
            content_length: conetnt_length,
            connection: connection,
        })
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HttpHandleStatus {
    Continue,
    EOF,
}

fn eof_err_helper<T>(e: std::io::Error) -> Result<T> {
    match e.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => {
            Err(Error::new(InternalError::EOFReached))
        }
        _ => Err(Error::new(e)),
    }
}

impl<R: AsyncRead, W: AsyncWrite> HttpHandler<R, W> {
    pub async fn handle(&mut self) -> Result<HttpHandleStatus> {
        self.internal_handle().await.or_else(|e| {
            if e.downcast_ref::<std::io::Error>()
                .map(|e| match e.kind() {
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => true,
                    _ => false,
                })
                .unwrap_or(false)
                || e.downcast_ref::<InternalError>()
                    .map(|e| *e == InternalError::EOFReached)
                    .unwrap_or(false)
            {
                Ok(HttpHandleStatus::EOF)
            } else {
                Err(e)
            }
        })
    }
    pub async fn internal_handle(&mut self) -> Result<HttpHandleStatus> {
        use HttpHandleStatus::*;
        let req_header = self.request_header().await;
        match req_header {
            Err(e) => {
                if e.downcast_ref::<InternalError>()
                    .map(|e| *e == InternalError::EOFReached)
                    .unwrap_or(false)
                {
                    return Err(e);
                }
                let content = self
                    .context
                    .cache_status_page
                    .get(&Status::InternalServerError)
                    .unwrap()
                    .clone();
                self.response_header(ResponseHeader {
                    status: Status::InternalServerError,
                    content_length: Some(content.len()),
                    content_type: "text/plain".to_string(),
                })
                .await?;
                self.conn_wr.write_all(&content).await?;
                Err(e)
            }
            Ok(req) => {
                if req.uri == "/echo" && req.method == Method::POST {
                    self.response_header(ResponseHeader {
                        status: Status::Ok,
                        content_length: None,
                        content_type: "text/plain".to_string(),
                    })
                    .await?;
                    self.echo_body(&req).await?;
                } else if req.method == Method::GET {
                    let path = self.context.serve_dir.join(Path::new(&req.uri[1..]));
                    let file: Option<File> = {
                        if path.starts_with(self.context.serve_dir.as_ref()) {
                            if let Ok(file) = fs::File::open(path).await {
                                if file.metadata().await.map(|metadata| metadata.is_file())? {
                                    Some(file)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                    if let Some(mut file) = file {
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
                        self.file_body(&mut file).await.map(|_| ())?
                    } else {
                        let content = self
                            .context
                            .cache_status_page
                            .get(&Status::NotFound)
                            .unwrap()
                            .clone();
                        self.response_header(ResponseHeader {
                            status: Status::NotFound,
                            content_length: Some(content.len()),
                            content_type: "text/html".to_string(),
                        })
                        .await?;
                        self.conn_wr.write_all(&content).await?;
                    }
                } else {
                    let content = self
                        .context
                        .cache_status_page
                        .get(&Status::MethodNotAllowed)
                        .unwrap()
                        .clone();
                    self.response_header(ResponseHeader {
                        status: Status::MethodNotAllowed,
                        content_length: Some(content.len()),
                        content_type: "text/html".to_string(),
                    })
                    .await?;
                    self.conn_wr.write_all(&content).await?;
                }
                self.conn_wr.flush().await?;
                if req.connection == ConnectionType::Close {
                    Ok(EOF)
                } else {
                    Ok(Continue)
                }
            }
        }
    }
    async fn request_header(&mut self) -> Result<RequestHeader> {
        let mut buf = String::with_capacity(1024);
        loop {
            self.conn_rd
                .read_line(&mut buf)
                .await
                .or_else(eof_err_helper)
                .and_then(|n| {
                    if n > 0 {
                        Ok(n)
                    } else {
                        Err(Error::new(InternalError::EOFReached))
                    }
                })?;
            if buf.ends_with("\r\n\r\n") {
                break;
            }
        }
        RequestHeader::parse(&buf)
    }
    async fn response_header(&mut self, resp: ResponseHeader) -> Result<()> {
        self.conn_wr
            .write(format!("HTTP/1.1 {} {}\r\n", resp.status as i32, resp.status).as_bytes())
            .await?;
        self.conn_wr.write(b"Server: Insomnia Server\r\n").await?;
        if let Some(sz) = resp.content_length {
            // if content length is set
            self.conn_wr
                .write(format!("Content-Length: {}\r\n", sz).as_bytes())
                .await?;
        }
        self.conn_wr
            .write(format!("Content-Type: {}\r\n", resp.content_type).as_bytes())
            .await?;
        self.conn_wr.write("\r\n".as_bytes()).await?;
        self.conn_wr.flush().await?;
        Ok(())
    }
    async fn file_body(&mut self, file: &mut File) -> Result<u64> {
        io::copy(file, &mut self.conn_wr)
            .await
            .map_err(|e| Error::new(e))
    }
    async fn echo_body(&mut self, req: &RequestHeader) -> Result<u64> {
        let mut trunc = self.conn_rd.as_mut().take(
            req.content_length
                .ok_or(InternalError::RequiredHeaderField)? as u64,
        );
        io::copy(&mut trunc, &mut self.conn_wr)
            .await
            .map_err(|e| Error::new(e))
    }
}
