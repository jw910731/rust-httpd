use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{self, io::AsyncWriteExt, net::TcpListener};
use tokio_task_pool::Pool;

mod http;
#[tokio::main]
async fn main() -> Result<()> {
    let pool = Pool::unbounded().with_run_timeout(Duration::from_secs(10));
    let listener = TcpListener::bind("localhost:8080").await?;
    println!("Bind on localhost:8080");
    let http_context = Arc::new(http::HttpContext::new(http::HttpHandleOption {
        status_page: HashMap::<http::Status, Box<Path>>::default(),
        serve_directory: Box::from(Path::new("./static/")),
    }));

    while let Ok((mut socket, _)) = listener.accept().await {
        let ctx = http_context.clone();
        pool.spawn(async move {
            let (mut rd, mut wr) = socket.split();
            let mut handler = ctx.get(&mut rd, &mut wr);
            use http::HttpHandleStatus::*;
            loop {
                match handler.handle().await {
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
                            .map_err(|e2| eprintln!("Log error failed {}", e2));
                    }
                    Ok(status) => {
                        if status == EOF {
                            break;
                        }
                    }
                }
            }
        })
        .await?;
    }
    Ok(())
}
