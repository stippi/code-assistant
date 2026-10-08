//! Wire types of the OpenAI Realtime protocol: the client events we send and
//! the subset of server events a voice agent reacts to.
//!
//! Server events not listed here parse as [`ServerEvent::Other`]; the
//! protocol sends many informational events (rate limits, content part
//! boundaries, buffer commits) a client can ignore.

use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

/// Sample rate of the PCM16 mono audio both directions use.
pub const SAMPLE_RATE: u32 = 24_000;

/// A function the model may call, as declared in the session.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema of the arguments object.
    pub parameters: Value,
}

/// The session settings sent with `session.update`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSettings {
    /// `None` when the endpoint routes to a fixed model.
    pub model: Option<String>,
    pub instructions: String,
    pub voice: String,
    pub tools: Vec<ToolDefinition>,
    /// The `turn_detection` object, e.g. `{"type": "semantic_vad"}`.
    pub turn_detection: Value,
    /// Model transcribing the user's speech; `None` turns transcription off.
    pub transcription_model: Option<String>,
}

/// An event the client sends.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    SessionUpdate(SessionSettings),
    /// Microphone samples (PCM16, 24 kHz, mono).
    AppendAudio(Vec<i16>),
    /// Ask the model to respond to the conversation as it stands.
    CreateResponse,
    /// Stop the response being generated.
    CancelResponse,
    /// Cut an assistant audio item down to what the user actually heard.
    TruncateItem {
        item_id: String,
        audio_end_ms: u64,
    },
    /// The result of a function call.
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    /// A text item from the system: context the model did not hear.
    SystemMessage(String),
}

impl ClientEvent {
    pub fn to_json(&self) -> Value {
        match self {
            ClientEvent::SessionUpdate(settings) => {
                let tools: Vec<Value> = settings
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        })
                    })
                    .collect();
                let mut input = json!({
                    "format": { "type": "audio/pcm", "rate": SAMPLE_RATE },
                    "turn_detection": settings.turn_detection,
                });
                if let Some(model) = &settings.transcription_model {
                    input["transcription"] = json!({ "model": model });
                }
                let mut event = json!({
                    "type": "session.update",
                    "session": {
                        "type": "realtime",
                        "instructions": settings.instructions,
                        "output_modalities": ["audio"],
                        "audio": {
                            "input": input,
                            "output": {
                                "format": { "type": "audio/pcm", "rate": SAMPLE_RATE },
                                "voice": settings.voice,
                            },
                        },
                        "tools": tools,
                        "tool_choice": "auto",
                    },
                });
                if let Some(model) = &settings.model {
                    event["session"]["model"] = json!(model);
                }
                event
            }
            ClientEvent::AppendAudio(samples) => json!({
                "type": "input_audio_buffer.append",
                "audio": encode_pcm16(samples),
            }),
            ClientEvent::CreateResponse => json!({ "type": "response.create" }),
            ClientEvent::CancelResponse => json!({ "type": "response.cancel" }),
            ClientEvent::TruncateItem {
                item_id,
                audio_end_ms,
            } => json!({
                "type": "conversation.item.truncate",
                "item_id": item_id,
                "content_index": 0,
                "audio_end_ms": audio_end_ms,
            }),
            ClientEvent::FunctionCallOutput { call_id, output } => json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                },
            }),
            ClientEvent::SystemMessage(text) => json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "message",
                    "role": "system",
                    "content": [{ "type": "input_text", "text": text }],
                },
            }),
        }
    }
}

/// An item of the conversation as the server reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConversationItem {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

/// The response a `response.created` / `response.done` event is about.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResponseInfo {
    #[serde(default)]
    pub id: String,
    /// `completed`, `cancelled`, `failed` or `incomplete` once done.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ErrorInfo {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
}

/// A server event. Event names follow the GA protocol; the beta names of
/// the audio events are accepted as aliases.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session.created")]
    SessionCreated,
    #[serde(rename = "session.updated")]
    SessionUpdated,
    #[serde(rename = "response.created")]
    ResponseCreated { response: ResponseInfo },
    #[serde(rename = "response.done")]
    ResponseDone { response: ResponseInfo },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: ConversationItem },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: ConversationItem },
    #[serde(rename = "response.output_audio.delta", alias = "response.audio.delta")]
    AudioDelta { item_id: String, delta: String },
    #[serde(
        rename = "response.output_audio_transcript.done",
        alias = "response.audio_transcript.done"
    )]
    AudioTranscriptDone { item_id: String, transcript: String },
    #[serde(rename = "input_audio_buffer.speech_started")]
    SpeechStarted,
    #[serde(rename = "input_audio_buffer.speech_stopped")]
    SpeechStopped,
    #[serde(rename = "conversation.item.input_audio_transcription.completed")]
    InputTranscription { item_id: String, transcript: String },
    #[serde(rename = "error")]
    Error { error: ErrorInfo },
    #[serde(other)]
    Other,
}

impl ServerEvent {
    /// Parse a text frame. A known event with an unexpected shape is
    /// reported as an error rather than silently dropped.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}

pub fn encode_pcm16(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_pcm16(data: &str) -> anyhow::Result<Vec<i16>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips() {
        let samples = vec![0, 1, -1, i16::MAX, i16::MIN, 1234];
        assert_eq!(decode_pcm16(&encode_pcm16(&samples)).unwrap(), samples);
    }

    #[test]
    fn parses_audio_delta_under_both_names() {
        for name in ["response.output_audio.delta", "response.audio.delta"] {
            let text =
                format!(r#"{{"type":"{name}","item_id":"i1","delta":"AAA=","response_id":"r"}}"#);
            assert_eq!(
                ServerEvent::parse(&text).unwrap(),
                ServerEvent::AudioDelta {
                    item_id: "i1".into(),
                    delta: "AAA=".into()
                }
            );
        }
    }

    #[test]
    fn unknown_events_parse_as_other() {
        let event = ServerEvent::parse(r#"{"type":"rate_limits.updated","rate_limits":[]}"#);
        assert_eq!(event.unwrap(), ServerEvent::Other);
    }

    #[test]
    fn parses_function_call_items() {
        let text = r#"{"type":"response.output_item.done","item":{"id":"it","type":"function_call",
            "name":"list_conversations","call_id":"c1","arguments":"{}","status":"completed"}}"#;
        let ServerEvent::OutputItemDone { item } = ServerEvent::parse(text).unwrap() else {
            panic!("wrong event");
        };
        assert_eq!(item.kind, "function_call");
        assert_eq!(item.call_id.as_deref(), Some("c1"));
        assert_eq!(item.name.as_deref(), Some("list_conversations"));
    }

    #[test]
    fn session_update_declares_tools_and_audio_formats() {
        let event = ClientEvent::SessionUpdate(SessionSettings {
            model: Some("gpt-realtime".into()),
            instructions: "be brief".into(),
            voice: "marin".into(),
            tools: vec![ToolDefinition {
                name: "t".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            turn_detection: json!({"type": "semantic_vad"}),
            transcription_model: Some("gpt-4o-mini-transcribe".into()),
        })
        .to_json();
        assert_eq!(event["session"]["tools"][0]["name"], "t");
        assert_eq!(event["session"]["model"], "gpt-realtime");
        assert_eq!(event["session"]["audio"]["input"]["format"]["rate"], 24000);
        assert_eq!(event["session"]["audio"]["output"]["voice"], "marin");
        assert_eq!(
            event["session"]["audio"]["input"]["transcription"]["model"],
            "gpt-4o-mini-transcribe"
        );
    }
}
