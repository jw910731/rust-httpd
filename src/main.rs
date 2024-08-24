use std::{collections::HashMap, env::args, path::Path, sync::Arc};

use anyhow::Result;
use log::{error, info};
use tokio::{self, net::TcpListener};
use tokio_task_pool::Pool;

mod http;
#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let pool = Pool::bounded(8);
    let bind_addr = args().nth(1).unwrap_or("0.0.0.0:8080".to_string());
    let listener = TcpListener::bind(&bind_addr).await?;
    info!("Bind on {}", bind_addr);
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
                        error!(
                            "serving request encounter error: {}\n{}\n",
                            e,
                            e.backtrace()
                        );
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
