use std::{borrow::Borrow, collections::HashMap, path::Path, sync::Arc};

use anyhow::Result;
use tokio::{self, io::AsyncWriteExt, net::TcpListener, sync::Mutex};

mod http;
#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("localhost:8080").await?;
    println!("Bind on localhost:8080");
    let http_context = Arc::new(http::HttpContext::new(http::HttpHandleOption {
        status_page: HashMap::<http::Status, Box<Path>>::default(),
        serve_directory: Box::from(Path::new("./static/")),
    }));

    while let Ok((mut socket, _)) = listener.accept().await {
        let ctx = http_context.clone();
        tokio::spawn(async move {
            let (mut rd, mut wr) = socket.split();
            let handler = ctx.get(&mut rd, &mut wr);
            let result = handler.handle().await;
            if let Err(e) = result {
                //eprintln!("serving request encounter error: {}\n{}", e, e.backtrace());
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
        });
    }
    Ok(())
}
