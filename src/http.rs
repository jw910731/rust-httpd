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

// Status represent the status code in the HTTP response
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

// Method represent the HTTP method in the request
#[derive(Clone, Copy, EnumIter, Debug, PartialEq, EnumString, Display)]
pub enum Method {
    GET,
    POST,
}

// Universal internal server erorr type
#[derive(Error, Debug, PartialEq)]
enum InternalError {
    // Error related to parse fail or malform request
    #[error("format error")]
    FormatError,

    // Missing required field error
    #[error("required header field is not provided")]
    RequiredHeaderField,

    // Speical error type when EOF is reached, this controls whether to shutdown current TCP connection
    #[error("eof reached without expectation")]
    EOFReached,

    // Not categorized error type
    #[error("unknown error")]
    Unknown,
}

// HTTP handler policy
pub struct HttpHandleOption {
    // Content to deliver when returning status code
    pub status_page: HashMap<Status, Box<Path>>,
    // Directory contain files to statically served
    pub serve_directory: Box<Path>,
}

// HTTP context shared in all http handler.
// Contains essential stateless readonly data for request handler
pub struct HttpContext {
    cache_status_page: HashMap<Status, Box<[u8]>>,
    serve_dir: Box<Path>,
}

// Internal fallback status page directory
static STATUS_PAGE_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/status_pages");

impl HttpContext {
    // New a `HttpContext` from `HttpHandleOption`
    pub fn new(options: HttpHandleOption) -> Self {
        let mut cache = HashMap::<Status, Box<[u8]>>::new();

        // Prepare HTTP error response page conetnt
        // FIXME: Do not ignore `option.status_page`
        for status in Status::iter() {
            let content = if let Some(f) =
                STATUS_PAGE_DIR.get_file(format!("{}.html", status as u32))
            {
                // If default status page exist
                Box::from(f.contents())
            } else {
                // Use simple response if no default status page is prepared
                Box::from(format!("<h1>{} {}<h1>", status as u32, status.to_string()).as_bytes())
            };
            cache.insert(status, content);
        }
        HttpContext {
            cache_status_page: cache,
            serve_dir: options.serve_directory,
        }
    }

    // New a `HttpHandler` using the current context
    pub fn get<R: AsyncRead, W: AsyncWrite>(self: &Self, rd: R, wr: W) -> HttpHandler<R, W> {
        HttpHandler {
            context: self,
            conn_rd: Box::pin(BufReader::new(rd)),
            conn_wr: Box::pin(BufWriter::new(wr)),
        }
    }
}

// Connection variant in HTTP `Connection` field
#[derive(Clone, Copy, Debug, PartialEq, EnumString)]
pub enum ConnectionType {
    #[strum(serialize = "keep-alive")]
    KeepAlive,
    #[strum(serialize = "close")]
    Close,
}

// HTTP client request header
// Currently Unrecognized fileds are ignored
struct RequestHeader {
    method: Method,
    uri: String,
    user_agent: Option<String>,
    host: Option<String>,
    content_length: Option<usize>,
    connection: ConnectionType,
}

impl RequestHeader {
    // Parse request header from raw string
    fn parse(raw: &str) -> Result<Self> {
        // Deal with the first line that contains HTTP method, request URI, and HTTP version (which is currently ignored)
        let mut line_iter = raw.split("\r\n");
        let mut iter = line_iter
            .next()
            .ok_or(InternalError::FormatError)?
            .split_ascii_whitespace();
        let method = Method::from_str(iter.next().ok_or(InternalError::FormatError)?)
            .map_err(|_| InternalError::FormatError)?;
        let uri = iter.next().ok_or(InternalError::FormatError)?.to_string();
        // FIXME: check http version

        // Deal with other header fields
        let mut host: Option<String> = None;
        let mut user_agent: Option<String> = None;
        let mut conetnt_length: Option<usize> = None;
        let mut connection = ConnectionType::KeepAlive;
        for line in line_iter {
            // Separate header key and header value
            let mut iter = line.split(": ").map(|sec| sec.replace("\r\n", ""));
            match iter.next().ok_or(InternalError::FormatError)?.as_str() {
                "Host" => {
                    host = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                "User-Agent" => {
                    user_agent = Some(iter.next().ok_or(InternalError::FormatError)?.to_string());
                }
                "Content-Length" => {
                    // Parse value as number
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
                // Ignore unrecognized fields
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

// This type contains stateful data for each HTTP TCP connection
// If encountering `Connection: keep-alive`, this object should be reused to get data of subsequent request from stateful buffer
// Note: Drop the BufReader will lose all buffered internal data and result in data loss, which is bad when `Connection: keep-alive`
pub struct HttpHandler<'a, R: AsyncRead, W: AsyncWrite> {
    context: &'a HttpContext,
    conn_rd: Pin<Box<BufReader<R>>>,
    conn_wr: Pin<Box<BufWriter<W>>>,
}

// How to handle the TCP connection after return from handle function
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HttpHandleStatus {
    Continue,
    EOF,
}

// Internal helper to convert IO Error into EOF Reached Error
fn eof_err_helper<T>(e: std::io::Error) -> Result<T> {
    match e.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => {
            Err(Error::new(InternalError::EOFReached))
        }
        _ => Err(Error::new(e)),
    }
}

impl<'a, R: AsyncRead, W: AsyncWrite> HttpHandler<'a, R, W> {
    // Handle function, the main entry point for the `HttpHandler` type
    pub async fn handle(&mut self) -> Result<HttpHandleStatus> {
        // Internally primarily dealing with error conversion into `HttpHandleStatus::EOF`
        self.internal_handle().await.or_else(|e| {
            // Borken pipe or connection reset should lead to connection close
            if e.downcast_ref::<std::io::Error>()
                .map(|e| match e.kind() {
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => true,
                    _ => false,
                })
                .unwrap_or(false)
                // EOF reached internal error variant should lead to connection close
                || e.downcast_ref::<InternalError>()
                    .map(|e| *e == InternalError::EOFReached)
                    .unwrap_or(false)
            {
                Ok(HttpHandleStatus::EOF)
            } else {
                // Propagate the error
                Err(e)
            }
        })
    }

    // The real handle function implementation
    async fn internal_handle(&mut self) -> Result<HttpHandleStatus> {
        use HttpHandleStatus::*;
        // Read and parse request
        let req_header = self.request_header().await;
        match req_header {
            Err(e) => {
                // If error is EOF reached variant
                if e.downcast_ref::<InternalError>()
                    .map(|e| *e == InternalError::EOFReached)
                    .unwrap_or(false)
                {
                    // FIXME: This should return `Ok(HttpHandleStatus::EOF)`, that would be more idiomatic
                    // Return EOF reached error to close connection
                    return Err(e);
                }

                // Get 500 intenal server error page content
                let content = self
                    .context
                    .cache_status_page
                    .get(&Status::InternalServerError)
                    .unwrap()
                    .clone();
                // Response to client with internal server content
                self.response_header(ResponseHeader {
                    status: Status::InternalServerError,
                    content_length: Some(content.len()),
                    content_type: "text/plain".to_string(),
                })
                .await?;
                self.conn_wr.write_all(&content).await?;

                // Propagate error
                Err(e)
            }
            Ok(req) => {
                // Route: See is POST of `/echo` or else
                if req.uri == "/echo" && req.method == Method::POST {
                    // HTTP Echo server

                    // Response with Ok first
                    self.response_header(ResponseHeader {
                        status: Status::Ok,
                        content_length: None,
                        content_type: "text/plain".to_string(),
                    })
                    .await?;

                    // Echo the content from client
                    self.echo_body(&req).await?;
                } else if req.method == Method::GET {
                    // Serve static file

                    // Get file path
                    let path = self.context.serve_dir.join(Path::new(&req.uri[1..]));

                    // None if file does not exist or out of serve directory
                    // Contains simple prevention of escape root serve dir
                    let file: Option<File> = {
                        // Escape check
                        if path.starts_with(self.context.serve_dir.as_ref()) {
                            // File existence check
                            if let Ok(file) = fs::File::open(path).await {
                                // Check is file not other
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

                    // If file is accessible
                    if let Some(mut file) = file {
                        // Rsponse with Ok
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

                        // Serve static file content
                        self.file_body(&mut file).await.map(|_| ())?
                    } else {
                        // Response 404 Not Found

                        // Prepare 404 page content
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
                    // Unknown method or other case
                    // FIXME: Should have a finer check instead of response with 405 at any route unmatch
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

                // IMPORTANT: Flush connection buffer
                // Client now can get response data
                self.conn_wr.flush().await?;

                // Leave the connection or not
                if req.connection == ConnectionType::Close {
                    Ok(EOF)
                } else {
                    Ok(Continue)
                }
            }
        }
    }

    // Read and parse request header
    async fn request_header(&mut self) -> Result<RequestHeader> {
        let mut buf = String::with_capacity(1024);

        // Read until \r\n\r\n
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

        // Parse
        RequestHeader::parse(&buf)
    }

    // Write response header
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

        // Early flush for the client to know server ready
        self.conn_wr.flush().await?;
        Ok(())
    }

    // Write file conetnt to connection
    async fn file_body(&mut self, file: &mut File) -> Result<u64> {
        io::copy(file, &mut self.conn_wr)
            .await
            .map_err(|e| Error::new(e))
    }

    // Echo content
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
