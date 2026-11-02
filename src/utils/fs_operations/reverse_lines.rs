//! Maps to CC `utils/fsOperations.ts:722-779#readLinesReverse`.
use futures::Stream;
use std::{collections::VecDeque, io, path::Path};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Raw fs access, independent of getFsImplementation, as in the source.
/// Dropping the stream closes the file just as returning from the generator does.
pub fn read_lines_reverse(path: &Path) -> impl Stream<Item = io::Result<String>> + Send + 'static {
    struct State {
        path: std::path::PathBuf,
        file: Option<tokio::fs::File>,
        position: u64,
        remainder: Vec<u8>,
        buffer: Vec<u8>,
        lines: VecDeque<String>,
        done: bool,
    }
    let state = State {
        path: path.to_owned(),
        file: None,
        position: 0,
        remainder: Vec::new(),
        buffer: vec![0; 4096],
        lines: VecDeque::new(),
        done: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        if state.done {
            if let Some(file) = state.file.take()
                && let Err(error) = super::handle::close(file.into_std().await)
            {
                return Some((
                    Err(super::error::native(error, "close", &state.path, None)),
                    state,
                ));
            }
            return None;
        }
        let result: io::Result<Option<String>> = async {
            if state.file.is_none() {
                let file = tokio::fs::File::open(&state.path)
                    .await
                    .map_err(|error| super::error::native(error, "open", &state.path, None))?;
                state.file = Some(file);
                state.position = state
                    .file
                    .as_ref()
                    .expect("opened above")
                    .metadata()
                    .await
                    .map_err(|error| super::error::native(error, "fstat", &state.path, None))?
                    .len();
            }
            loop {
                if let Some(line) = state.lines.pop_front() {
                    return Ok(Some(line));
                }
                if state.position == 0 {
                    let remainder = std::mem::take(&mut state.remainder);
                    state.done = true;
                    return Ok((!remainder.is_empty())
                        .then(|| String::from_utf8_lossy(&remainder).into_owned()));
                }
                let size = state.position.min(4096) as usize;
                state.position -= size as u64;
                let file = state.file.as_mut().expect("opened above");
                file.seek(io::SeekFrom::Start(state.position)).await?;
                // Source ignores bytesRead and retains the reusable buffer's tail.
                file.read(&mut state.buffer[..size])
                    .await
                    .map_err(|error| super::error::native(error, "read", &state.path, None))?;
                let mut combined = state.buffer[..size].to_vec();
                combined.append(&mut state.remainder);
                if let Some(newline) = combined.iter().position(|byte| *byte == b'\n') {
                    state.remainder = combined[..newline].to_vec();
                    state.lines = String::from_utf8_lossy(&combined[newline + 1..])
                        .split('\n')
                        .rev()
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect();
                } else {
                    state.remainder = combined;
                }
            }
        }
        .await;
        match result {
            Ok(Some(line)) => Some((Ok(line), state)),
            Ok(None) => {
                if let Some(file) = state.file.take()
                    && let Err(error) = super::handle::close(file.into_std().await)
                {
                    state.done = true;
                    return Some((
                        Err(super::error::native(error, "close", &state.path, None)),
                        state,
                    ));
                }
                None
            }
            Err(error) => {
                state.done = true;
                let error = if let Some(file) = state.file.take() {
                    super::handle::close(file.into_std().await)
                        .err()
                        .map(|error| super::error::native(error, "close", &state.path, None))
                        .unwrap_or(error)
                } else {
                    error
                };
                Some((Err(error), state))
            }
        }
    })
}
