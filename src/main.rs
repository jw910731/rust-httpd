use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::Result;
use tokio::{self, io::AsyncWriteExt, net::TcpListener};
use tokio_task_pool::Pool;

mod http;
#[tokio::main]
async fn main() -> Result<()> {
    let pool = Pool::bounded(8);
    let listener = TcpListener::bind("localhost:8080").await?;
    println!("Bind on localhost:8080");
    let http_context = Arc::new(http::HttpContext::new(http::HttpHandleOption {
        status_page: HashMap::<http::Status, Box<Path>>::default(),
        serve_directory: Box::from(Path::new("./static/")),
    }));

    while let Ok((mut socket, _)) = listener.accept().await {
        // For each TCP connection

        // Get a ref of HTTP Context
        let ctx = http_context.clone();

        // Spawn a task dedicated to the connection
        pool.spawn(async move {
            // Split read and write handle
            let (mut rd, mut wr) = socket.split();

            // Get handler object from rw handle
            let mut handler = ctx.get(&mut rd, &mut wr);
            use http::HttpHandleStatus::*;

            // For each http request
            loop {
                // Deal with the request
                match handler.handle().await {
                    // Error occurred
                    Err(e) => {
                        let _ = tokio::io::stderr()
                            .write(
                                format!(
                                    "serving request encounter error: {}\n{}\n",
                                    e,
                                    e.backtrace()
                                )
                                .as_bytes(),
                            )
                            .await
                            // If async write to stderr fails, fallback to synchronous write
                            .map_err(|e2| eprintln!("Log error failed {}", e2));
                    }
                    Ok(status) => {
                        // Disconnect if needed
                        if status == EOF {
                            break;
                        }
                    }
                }
            }
        })
        // Await until task spawned
        .await?;
    }
    Ok(())
}
