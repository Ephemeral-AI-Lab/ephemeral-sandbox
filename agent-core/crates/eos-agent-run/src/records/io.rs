use std::path::Path;

use eos_types::{ContentBlock, Message, MessageRole};
use eos_types::{JsonObject, UtcDateTime};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::error::{MessageRecordError, Result};
use super::record::{MessageAppendRange, NodeEvent, RecordBytes};

#[derive(Serialize)]
struct MessageRow<'a> {
    #[serde(rename = "type")]
    row_type: &'static str,
    role: &'static str,
    content: &'a [ContentBlock],
}

#[derive(Serialize)]
struct MessageRowOwned {
    #[serde(rename = "type")]
    row_type: &'static str,
    role: &'static str,
    content: Vec<ContentBlock>,
}

pub(crate) async fn append_message_rows(
    path: &Path,
    row_type: &'static str,
    messages: &[Message],
) -> Result<MessageAppendRange> {
    let rows: Vec<_> = messages
        .iter()
        .map(|message| MessageRow {
            row_type,
            role: role_wire(message.role),
            content: &message.content,
        })
        .collect();
    append_rows(path, &rows).await
}

pub(crate) async fn append_initial_message_rows(
    path: &Path,
    system_prompt: &str,
    initial_messages: &[Message],
) -> Result<MessageAppendRange> {
    let mut rows = Vec::with_capacity(initial_messages.len().saturating_add(1));
    rows.push(MessageRowOwned {
        row_type: "initial_message",
        role: "system",
        content: vec![ContentBlock::Text {
            text: system_prompt.to_owned(),
        }],
    });
    rows.extend(initial_messages.iter().map(|message| MessageRowOwned {
        row_type: "initial_message",
        role: role_wire(message.role),
        content: message.content.clone(),
    }));
    append_rows(path, &rows).await
}

async fn append_rows<T: Serialize>(path: &Path, rows: &[T]) -> Result<MessageAppendRange> {
    let start_byte = file_len_or_zero(path).await?;
    if rows.is_empty() {
        return Ok(MessageAppendRange {
            count: 0,
            start_byte,
            end_byte: start_byte,
        });
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    for row in rows {
        let line = serde_json::to_string(row)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }
    file.flush().await?;
    let end_byte = file_len_or_zero(path).await?;
    Ok(MessageAppendRange {
        count: rows.len(),
        start_byte,
        end_byte,
    })
}

pub(crate) async fn append_event(path: &Path, kind: String, payload: JsonObject) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let seq = next_event_seq(path).await?;
    let event = NodeEvent {
        seq,
        kind,
        payload,
        created_at: UtcDateTime::now(),
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let line = serde_json::to_string(&event)?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

async fn next_event_seq(path: &Path) -> Result<u64> {
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => {
            let last_seq = raw
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(serde_json::from_str::<NodeEvent>)
                .transpose()?
                .map_or(0, |event| event.seq);
            Ok(last_seq.saturating_add(1))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(1),
        Err(err) => Err(err.into()),
    }
}

pub(crate) async fn read_bytes_after(path: &Path, after_byte: u64) -> Result<RecordBytes> {
    let mut file = tokio::fs::File::open(path).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            MessageRecordError::missing_path(path)
        } else {
            MessageRecordError::Io(err)
        }
    })?;
    let len = file.metadata().await?.len();
    if after_byte > len {
        return Err(MessageRecordError::OffsetOutOfRange {
            offset: after_byte,
            len,
        });
    }
    file.seek(std::io::SeekFrom::Start(after_byte)).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    Ok(RecordBytes {
        bytes,
        next_byte_offset: len,
    })
}

pub(crate) async fn read_events_after(path: &Path, after_seq: u64) -> Result<Vec<NodeEvent>> {
    let raw = tokio::fs::read_to_string(path).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            MessageRecordError::missing_path(path)
        } else {
            MessageRecordError::Io(err)
        }
    })?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<NodeEvent>)
        .filter_map(|result| match result {
            Ok(event) if event.seq > after_seq => Some(Ok(event)),
            Ok(_) => None,
            Err(err) => Some(Err(MessageRecordError::Json(err))),
        })
        .collect()
}

async fn file_len_or_zero(path: &Path) -> Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

fn role_wire(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
