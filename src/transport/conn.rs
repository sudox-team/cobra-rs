use tokio::net::{TcpStream, ToSocketAddrs};
use std::io;
use std::sync::Arc;

use crate::transport::pool::{PoolAny, Pool, WriteError};
use crate::transport::buffer::ConcatBuffer;
use crate::transport::frame::Frame;
use tokio::sync::Notify;

pub struct Conn {
    write_pool: PoolAny<Frame>,
    read_pool: Pool<u8, Frame>,
    conn_close_notifier: Arc<Notify>,
}

impl Conn {
    pub(crate) async fn from_raw(tcp_stream: TcpStream,
                                 server_close_notifier: Option<Arc<Notify>>) -> Self {
        let read_tcp_stream = Arc::new(tcp_stream);
        let write_tcp_stream = read_tcp_stream.clone();

        let read_pool = Pool::new();
        let write_pool = PoolAny::new();

        let buffer = ConcatBuffer::default();

        let conn_close_notifier = Arc::new(Notify::new());

        tokio::spawn(Conn::close_task(
            server_close_notifier,
            conn_close_notifier.clone(),
            read_pool.clone(),
            write_pool.clone()
        ));

        tokio::spawn(Conn::read_loop(
            read_tcp_stream,
            read_pool.clone(),
            buffer
        ));

        tokio::spawn(Conn::write_loop(
            write_tcp_stream,
            write_pool.clone()
        ));

        Conn {
            write_pool,
            read_pool,
            conn_close_notifier,
        }
    }

    pub async fn connect<T: ToSocketAddrs>(addr: T) -> io::Result<Self> {
        Ok(
            Conn::from_raw(TcpStream::connect(addr).await?, None)
                .await
        )
    }

    async fn close_task(server_close_notifier: Option<Arc<Notify>>,
                        conn_close_notifier: Arc::<Notify>,
                        read_pool: Pool<u8, Frame>,
                        write_pool: PoolAny<Frame>) {
        match server_close_notifier {
            Some(server_close_notifier) => {
                tokio::select! {
                    _ = server_close_notifier.notified() => {}
                    _ = conn_close_notifier.notified() => {}
                }
            }
            None => {
                conn_close_notifier.notified().await;
            }
        }
        read_pool.close().await;
        write_pool.close().await;
    }

    async fn read_loop(read_tcp_stream: Arc<TcpStream>,
                       read_pool: Pool<u8, Frame>,
                       mut buffer: ConcatBuffer<Frame>) {
        loop {
            if read_tcp_stream.readable().await.is_err() {
                break;
            }

            match read_tcp_stream.try_read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(_) => break
            }

            while let Some(chunk) = buffer.try_read_chunk() {
                if read_pool.write(chunk).await.is_err() {
                    break;
                }
            }
        }
        read_pool.close().await;
    }

    async fn write_loop(write_tcp_stream: Arc<TcpStream>,
                        write_pool: PoolAny<Frame>) {
        while let Some(frame) = write_pool.read().await {
            if write_tcp_stream.writable().await.is_err() {
                break;
            }

            match write_tcp_stream.try_write(&frame) {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(_) => break
            }
        }
        write_pool.close().await;
    }

    // Return None if connection close
    pub async fn read(&self, kind: u8) -> Option<Frame> {
        self.read_pool.read(kind).await
    }

    // Return WriteError<F> if connection close
    pub async fn write(&self, frame: Frame) -> Result<(), WriteError<Frame>> {
        self.write_pool.write(frame).await
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.conn_close_notifier.notify_one();
    }
}
