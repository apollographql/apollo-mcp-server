use rmcp::RoleServer;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use std::io::{ErrorKind, Write};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

pub struct NonBlockStdIo {
    tx_out: tokio::sync::mpsc::Sender<TxJsonRpcMessage<RoleServer>>,
    rx_in: tokio::sync::mpsc::Receiver<RxJsonRpcMessage<RoleServer>>,
}

impl NonBlockStdIo {
    pub fn new(cancellation_token: CancellationToken) -> Self {
        let (tx_in, rx_in) = tokio::sync::mpsc::channel(100);
        let (tx_out, mut rx_out) = tokio::sync::mpsc::channel(100);

        let stdin_cancel_token = cancellation_token.clone();
        let stdout_cancel_token = cancellation_token.clone();

        std::thread::spawn(move || {
            for line_result in std::io::stdin().lines() {
                let line = match line_result {
                    Ok(line) => line,
                    Err(e) => {
                        error!("[Proxy] Failed to read from stdin: {e:?}");
                        stdin_cancel_token.cancel();
                        break;
                    }
                };

                debug!("[Proxy] Stdin received: {line}");

                let data = match serde_json::from_slice(line.as_bytes()) {
                    Ok(data) => data,
                    Err(e) => {
                        error!("[Proxy] Failed to deserialize json: {e:?}");
                        stdin_cancel_token.cancel();
                        break;
                    }
                };

                match tx_in.blocking_send(data) {
                    Ok(_) => {}
                    Err(e) => {
                        error!("[Proxy] Failed to send data: {e:?}");
                        stdin_cancel_token.cancel();
                        break;
                    }
                }
            }
        });

        std::thread::spawn(move || {
            loop {
                if let Some(data) = rx_out.blocking_recv() {
                    let data = serde_json::to_string(&data).unwrap_or_else(|e| {
                        error!("[Proxy] Couldn't serialize data: {e:?}");
                        stdout_cancel_token.cancel();
                        "".to_string()
                    }) + "\n";

                    match std::io::stdout().write_all(data.as_bytes()) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("[Proxy] Failed to write data to stdout: {e:?}");
                            stdout_cancel_token.cancel();
                            break;
                        }
                    }

                    match std::io::stdout().flush() {
                        Ok(_) => {}
                        Err(e) => {
                            error!("[Proxy] Failed to flush stdout: {e:?}");
                            stdout_cancel_token.cancel();
                            break;
                        }
                    }
                }
            }
        });

        Self { tx_out, rx_in }
    }
}

impl Transport<RoleServer> for NonBlockStdIo {
    type Error = tokio::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.tx_out.clone();

        async move {
            debug!("Sending message to server: {item:?}");
            tx.send(item).await.map_err(|e| {
                tokio::io::Error::new(
                    ErrorKind::BrokenPipe,
                    format!("NonBlockStdIo send error: {e:?}"),
                )
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleServer>>> + Send {
        async move {
            let data = self.rx_in.recv().await;
            debug!("[NonBlockStdIo receiving] {data:?}");
            data
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async move {
            debug!("[NonBlockStdIo] Closing connection");
            self.rx_in.close();
            Ok(())
        }
    }
}

impl Drop for NonBlockStdIo {
    fn drop(&mut self) {
        debug!("[NonBlockStdIo] Dropping connection");
    }
}
