use pin_project_lite::pin_project;
use std::{cmp::Ordering, fs::Metadata, path::PathBuf, pin::Pin, task::Poll};

use futures::{pin_mut, ready, Future, FutureExt, Stream};
use tokio::{
    fs::{self, File},
    io::{self, AsyncBufReadExt, BufReader},
    time::{sleep, Duration, Sleep},
};

pin_project! {
    pub struct FollowFile {
        inner: Inner,
        state: State,
    }
}

pub enum State {
    Open(Pin<Box<dyn Future<Output = io::Result<Inner>> + Send>>),
    Read,
    Sleep(Pin<Box<Sleep>>),
    CheckLength(Pin<Box<dyn Future<Output = io::Result<Metadata>> + Send>>),
}

pub struct Inner {
    path: PathBuf,
    buffer: Vec<u8>,
    reader: BufReader<File>,
    len: u64,
    pos: usize,
}

impl FollowFile {
    pub async fn new(path: PathBuf) -> io::Result<Self> {
        let inner = Inner::new(path).await?;
        Ok(FollowFile {
            inner,
            state: State::Read,
        })
    }
}

impl Inner {
    pub async fn new(path: PathBuf) -> io::Result<Self> {
        let metadata = fs::metadata(&path).await?;
        let len = metadata.len();
        let file = File::open(&path).await?;
        let reader = BufReader::new(file);
        Ok(Inner {
            path,
            buffer: Vec::new(),
            reader,
            len,
            pos: 0,
        })
    }
}

impl Stream for FollowFile {
    type Item = io::Result<(usize, String)>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.project();
        let state = this.state;
        let inner = this.inner;
        loop {
            let (new_state, maybe_line) = match state {
                State::Open(future) => match ready!(future.poll_unpin(cx)) {
                    Ok(new_inner) => {
                        *inner = new_inner;
                        (State::Read, None)
                    }
                    Err(_) => (State::Sleep(Box::pin(sleep(Duration::from_secs(1)))), None),
                },
                State::Read => {
                    let buffer = &mut inner.buffer;
                    let readline = inner.reader.read_until(b'\n', buffer);
                    pin_mut!(readline);
                    match ready!(readline.poll_unpin(cx)) {
                        Ok(0) => {
                            // EOF - wait for new bytes
                            (State::Sleep(Box::pin(sleep(Duration::from_secs(1)))), None)
                        }
                        Ok(_) => {
                            if buffer[buffer.len() - 1] == b'\n' {
                                // line complete
                                inner.pos += 1;
                                let mut result = Vec::new();
                                // remove \n
                                buffer.pop();
                                std::mem::swap(buffer, &mut result);
                                let line = String::from_utf8(result).map_err(|_| {
                                    io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "line did not contain valid UTF-8",
                                    )
                                });
                                (State::Read, Some(line.map(|l| (inner.pos, l))))
                            } else {
                                // new bytes, but no newline - continue reading
                                (State::Read, None)
                            }
                        }
                        Err(err) => (State::Read, Some(Err(err))),
                    }
                }
                State::Sleep(future) => {
                    ready!(future.poll_unpin(cx));
                    let metadata = Box::pin(fs::metadata(inner.path.to_owned()));
                    (State::CheckLength(metadata), None)
                }
                State::CheckLength(future) => match ready!(future.poll_unpin(cx)) {
                    Ok(metadata) => {
                        let new_len = metadata.len();
                        match inner.len.cmp(&new_len) {
                            Ordering::Less => {
                                // file got larger, continue reading
                                inner.len = new_len;
                                (State::Read, None)
                            }
                            Ordering::Equal => {
                                // file still has the same length
                                (State::Sleep(Box::pin(sleep(Duration::from_secs(1)))), None)
                            }
                            Ordering::Greater => {
                                // file truncated, start from the beginning
                                (State::Open(Box::pin(Inner::new(inner.path.clone()))), None)
                            }
                        }
                    }
                    Err(_) => (State::Sleep(Box::pin(sleep(Duration::from_secs(1)))), None),
                },
            };

            *state = new_state;

            if let Some(line) = maybe_line {
                return Poll::Ready(Some(line));
            }
        }
    }
}
