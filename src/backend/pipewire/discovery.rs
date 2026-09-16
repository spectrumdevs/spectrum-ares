use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use pipewire as pw;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

const SPOTIFY_MATCHERS: &[&str] = &[
    "spotify",
    "com.spotify.client",
    "spotify_player",
    "spotifyd",
    "spotify-launcher",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceClassification {
    ClientOnly,
    PlaybackNode,
    Stream,
    LinkedSpotifyPlaybackNode,
    Unknown,
}

impl fmt::Display for SourceClassification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientOnly => formatter.write_str("ClientOnly"),
            Self::PlaybackNode => formatter.write_str("PlaybackNode"),
            Self::Stream => formatter.write_str("Stream"),
            Self::LinkedSpotifyPlaybackNode => formatter.write_str("LinkedSpotifyPlaybackNode"),
            Self::Unknown => formatter.write_str("Unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSource {
    pub id: u32,
    pub object_serial: Option<u64>,
    pub object_type: String,
    pub client_id: Option<u32>,
    pub linked_client_id: Option<u32>,
    pub linked_client_name: Option<String>,
    pub node_name: Option<String>,
    pub node_description: Option<String>,
    pub application_name: Option<String>,
    pub application_process_binary: Option<String>,
    pub client_name: Option<String>,
    pub media_name: Option<String>,
    pub media_class: Option<String>,
    pub media_role: Option<String>,
    pub media_category: Option<String>,
    pub media_type: Option<String>,
    pub target_object: Option<String>,
    pub node_target: Option<String>,
}

impl DiscoveredSource {
    pub fn display_name(&self) -> &str {
        self.node_name
            .as_deref()
            .or(self.node_description.as_deref())
            .or(self.media_name.as_deref())
            .or(self.application_name.as_deref())
            .or(self.client_name.as_deref())
            .or(self.linked_client_name.as_deref())
            .unwrap_or("<unnamed>")
    }

    pub fn is_audio_relevant(&self) -> bool {
        self.media_class
            .as_deref()
            .is_some_and(|value| contains_ascii_ci(value, "audio"))
            || eq_ascii_ci(self.media_type.as_deref(), Some("audio"))
            || any_contains_ascii_ci(
                [
                    self.media_name.as_deref(),
                    self.node_description.as_deref(),
                    self.node_name.as_deref(),
                ],
                "audio",
            )
    }

    pub fn is_direct_spotify_match(&self) -> bool {
        let fields = [
            self.application_name.as_deref(),
            self.application_process_binary.as_deref(),
            self.client_name.as_deref(),
            self.media_name.as_deref(),
            self.node_name.as_deref(),
            self.node_description.as_deref(),
        ];

        fields
            .iter()
            .copied()
            .flatten()
            .any(|field| matches_spotify(Some(field)))
    }

    pub fn is_linked_to_spotify_client(&self) -> bool {
        self.linked_client_id.is_some()
    }

    pub fn is_spotify_related(&self) -> bool {
        self.is_direct_spotify_match() || self.is_linked_to_spotify_client()
    }

    pub fn is_stream_like(&self) -> bool {
        self.media_class
            .as_deref()
            .is_some_and(|value| value.starts_with("Stream/"))
            || self.object_type == "EndpointStream"
    }

    pub fn looks_like_audio_playback_stream(&self) -> bool {
        self.is_audio_relevant()
            && (self.media_class.as_deref().is_some_and(|value| {
                contains_ascii_ci(value, "stream/output/audio")
                    || (value.starts_with("Stream/") && contains_ascii_ci(value, "output"))
            }) || (self.is_stream_like()
                && eq_ascii_ci(self.media_category.as_deref(), Some("playback"))))
    }

    pub fn looks_like_playback_node(&self) -> bool {
        self.looks_like_audio_playback_stream()
            || (self.is_audio_relevant()
                && (self
                    .media_class
                    .as_deref()
                    .is_some_and(|value| contains_ascii_ci(value, "audio/sink"))
                    || eq_ascii_ci(self.media_category.as_deref(), Some("playback"))))
    }

    pub fn classification(&self) -> SourceClassification {
        if self.is_linked_to_spotify_client() && self.looks_like_audio_playback_stream() {
            SourceClassification::LinkedSpotifyPlaybackNode
        } else if self.object_type == "Client" && self.is_direct_spotify_match() {
            SourceClassification::ClientOnly
        } else if self.looks_like_audio_playback_stream() {
            SourceClassification::Stream
        } else if self.looks_like_playback_node() {
            SourceClassification::PlaybackNode
        } else {
            SourceClassification::Unknown
        }
    }

    pub fn spotify_candidate_score(&self) -> i32 {
        if !self.is_spotify_related() {
            return 0;
        }

        let mut score = 0;

        if self.is_linked_to_spotify_client() && self.looks_like_audio_playback_stream() {
            score += 1000;
        } else if self.is_direct_spotify_match() && self.looks_like_audio_playback_stream() {
            score += 850;
        } else if self.is_direct_spotify_match() && self.looks_like_playback_node() {
            score += 700;
        } else if self.object_type == "Client" && self.is_direct_spotify_match() {
            score += 250;
        } else if self.is_linked_to_spotify_client() {
            score += 200;
        }

        if self.is_direct_spotify_match() {
            score += 80;
        }
        if self.is_stream_like() {
            score += 40;
        }
        if eq_ascii_ci(self.media_category.as_deref(), Some("playback")) {
            score += 30;
        }
        if eq_ascii_ci(self.media_role.as_deref(), Some("music")) {
            score += 25;
        }
        if self.target_object.is_some() || self.node_target.is_some() {
            score += 10;
        }
        if self.object_serial.is_some() {
            score += 5;
        }

        score
    }

    pub fn is_capture_eligible(&self) -> bool {
        self.looks_like_audio_playback_stream() && self.is_spotify_related()
    }

    pub fn capture_eligibility_reason(&self) -> String {
        if self.is_linked_to_spotify_client() && self.looks_like_audio_playback_stream() {
            let linked_name = self
                .linked_client_name
                .as_deref()
                .unwrap_or("<unknown client>");
            return format!(
                "capture-eligible: audio playback stream linked to Spotify client {} ({linked_name}) via client.id",
                self.linked_client_id.unwrap_or_default()
            );
        }

        if self.is_direct_spotify_match() && self.looks_like_audio_playback_stream() {
            return format!(
                "capture-eligible: audio playback stream directly matches Spotify metadata on {}",
                self.direct_match_fields().join(", ")
            );
        }

        if self.object_type == "Client" && self.is_direct_spotify_match() {
            return "not capture-eligible: Spotify client was found, but no linked playback stream/node was discovered".to_string();
        }

        if !self.looks_like_audio_playback_stream() {
            return "not capture-eligible: object is not an audio playback stream/node".to_string();
        }

        "not capture-eligible: playback stream is not linked to a Spotify client and has no Spotify-identifying metadata".to_string()
    }

    pub fn selection_reason(&self) -> String {
        let mut reasons = Vec::new();

        if self.is_linked_to_spotify_client() {
            let linked_name = self
                .linked_client_name
                .as_deref()
                .unwrap_or("<unknown client>");
            reasons.push(format!(
                "linked to Spotify client {} ({linked_name}) via client.id",
                self.linked_client_id.unwrap_or_default()
            ));
        }

        if self.is_direct_spotify_match() {
            reasons.push(format!(
                "direct Spotify metadata match on {}",
                self.direct_match_fields().join(", ")
            ));
        }

        if self.looks_like_audio_playback_stream() {
            reasons.push("looks like an audio playback stream/node".to_string());
        } else if self.looks_like_playback_node() {
            reasons.push("looks like an audio playback node but not a stream".to_string());
        }

        if eq_ascii_ci(self.media_category.as_deref(), Some("playback")) {
            reasons.push("media.category=Playback".to_string());
        }
        if eq_ascii_ci(self.media_role.as_deref(), Some("music")) {
            reasons.push("media.role=music".to_string());
        }
        if let Some(target_object) = self.target_object.as_deref() {
            reasons.push(format!("target.object={target_object}"));
        }
        if let Some(node_target) = self.node_target.as_deref() {
            reasons.push(format!("node.target={node_target}"));
        }

        if reasons.is_empty() {
            reasons.push("no Spotify-specific indicators were found".to_string());
        }

        format!(
            "score {}: {}",
            self.spotify_candidate_score(),
            reasons.join("; ")
        )
    }

    fn direct_match_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();

        if matches_spotify(self.application_name.as_deref()) {
            fields.push("application.name");
        }
        if matches_spotify(self.application_process_binary.as_deref()) {
            fields.push("application.process.binary");
        }
        if matches_spotify(self.client_name.as_deref()) {
            fields.push("client.name");
        }
        if matches_spotify(self.media_name.as_deref()) {
            fields.push("media.name");
        }
        if matches_spotify(self.node_name.as_deref()) {
            fields.push("node.name");
        }
        if matches_spotify(self.node_description.as_deref()) {
            fields.push("node.description");
        }

        fields
    }
}

#[derive(Debug)]
pub enum PipeWireDiscoveryError {
    PipeWire(String),
}

impl fmt::Display for PipeWireDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PipeWire(message) => formatter.write_str(message),
        }
    }
}

impl Error for PipeWireDiscoveryError {}

pub fn discover_sources() -> Result<Vec<DiscoveredSource>, PipeWireDiscoveryError> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| PipeWireDiscoveryError::PipeWire(format!("create main loop: {err}")))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| PipeWireDiscoveryError::PipeWire(format!("create context: {err}")))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| PipeWireDiscoveryError::PipeWire(format!("connect to core: {err}")))?;
    let registry = core
        .get_registry()
        .map_err(|err| PipeWireDiscoveryError::PipeWire(format!("get registry: {err}")))?;

    let discovered = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));

    let pending = core
        .sync(0)
        .map_err(|err| PipeWireDiscoveryError::PipeWire(format!("sync registry: {err}")))?;

    let core_done = done.clone();
    let core_loop = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                core_done.set(true);
                core_loop.quit();
            }
        })
        .register();

    let registry_sources = discovered.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(source) = source_from_global(global) {
                debug_log(&format!(
                    "discovered PipeWire {} id={} client_id={:?} name={} direct_spotify={}",
                    source.object_type,
                    source.id,
                    source.client_id,
                    source.display_name(),
                    source.is_direct_spotify_match()
                ));
                registry_sources.borrow_mut().push(source);
            }
        })
        .register();

    while !done.get() {
        mainloop.run();
    }

    let mut sources = discovered.borrow().clone();
    resolve_spotify_links(&mut sources);
    sources.sort_by_key(|source| (source.object_type.clone(), source.id));

    for source in &sources {
        debug_log(&format!(
            "resolved {} id={} class={} score={} linked_client_id={:?} eligible={}",
            source.object_type,
            source.id,
            source.classification(),
            source.spotify_candidate_score(),
            source.linked_client_id,
            source.is_capture_eligible()
        ));
    }

    Ok(sources)
}

#[allow(dead_code)]
pub fn spotify_candidates() -> Result<Vec<DiscoveredSource>, PipeWireDiscoveryError> {
    Ok(discover_sources()?
        .into_iter()
        .filter(DiscoveredSource::is_spotify_related)
        .collect())
}

#[allow(dead_code)]
pub fn find_best_spotify_source() -> Result<Option<DiscoveredSource>, PipeWireDiscoveryError> {
    Ok(select_best_spotify_source(&discover_sources()?))
}

pub fn select_best_spotify_source(sources: &[DiscoveredSource]) -> Option<DiscoveredSource> {
    sources
        .iter()
        .filter(|source| source.spotify_candidate_score() > 0)
        .max_by_key(|source| (source.spotify_candidate_score(), source.id))
        .cloned()
}

fn source_from_global<P>(global: &GlobalObject<P>) -> Option<DiscoveredSource>
where
    P: AsRef<DictRef>,
{
    if !is_relevant_object_type(&global.type_) {
        return None;
    }

    let props = global.props.as_ref().map(AsRef::as_ref);
    let source = DiscoveredSource {
        id: global.id,
        object_serial: property_u64(props, "object.serial"),
        object_type: object_type_name(&global.type_).to_string(),
        client_id: property_u32(props, "client.id"),
        linked_client_id: None,
        linked_client_name: None,
        node_name: property(props, "node.name"),
        node_description: property(props, "node.description"),
        application_name: property(props, "application.name"),
        application_process_binary: property(props, "application.process.binary"),
        client_name: first_property(props, &["client.name", "application.name"]),
        media_name: property(props, "media.name"),
        media_class: property(props, "media.class"),
        media_role: property(props, "media.role"),
        media_category: property(props, "media.category"),
        media_type: property(props, "media.type"),
        target_object: first_property(props, &["target.object", "target.node"]),
        node_target: property(props, "node.target"),
    };

    if should_keep_source(&source) {
        Some(source)
    } else {
        None
    }
}

fn resolve_spotify_links(sources: &mut [DiscoveredSource]) {
    let spotify_clients: HashMap<u32, String> = sources
        .iter()
        .filter(|source| source.object_type == "Client" && source.is_direct_spotify_match())
        .map(|source| (source.id, source.display_name().to_string()))
        .collect();

    for source in sources.iter_mut() {
        if let Some(client_id) = source.client_id {
            if let Some(client_name) = spotify_clients.get(&client_id) {
                source.linked_client_id = Some(client_id);
                source.linked_client_name = Some(client_name.clone());
            }
        }
    }
}

fn should_keep_source(source: &DiscoveredSource) -> bool {
    source.object_type == "Client"
        || source.is_audio_relevant()
        || source.is_direct_spotify_match()
        || source.client_id.is_some()
        || source.target_object.is_some()
        || source.node_target.is_some()
}

fn is_relevant_object_type(object_type: &ObjectType) -> bool {
    matches!(
        object_type,
        ObjectType::Node
            | ObjectType::Client
            | ObjectType::ClientNode
            | ObjectType::ClientSession
            | ObjectType::EndpointStream
    )
}

fn object_type_name(object_type: &ObjectType) -> &str {
    match object_type {
        ObjectType::Node => "Node",
        ObjectType::Client => "Client",
        ObjectType::ClientNode => "ClientNode",
        ObjectType::ClientSession => "ClientSession",
        ObjectType::EndpointStream => "EndpointStream",
        other => other.to_str(),
    }
}

fn property(props: Option<&DictRef>, key: &str) -> Option<String> {
    props
        .and_then(|props| props.get(key))
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn property_u64(props: Option<&DictRef>, key: &str) -> Option<u64> {
    props
        .and_then(|props| props.parse::<u64>(key))
        .and_then(Result::ok)
}

fn property_u32(props: Option<&DictRef>, key: &str) -> Option<u32> {
    props
        .and_then(|props| props.parse::<u32>(key))
        .and_then(Result::ok)
}

fn first_property(props: Option<&DictRef>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| property(props, key))
}

fn any_contains_ascii_ci(values: [Option<&str>; 3], needle: &str) -> bool {
    values
        .into_iter()
        .flatten()
        .any(|value| contains_ascii_ci(value, needle))
}

fn matches_spotify(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let lowered = value.to_ascii_lowercase();

        SPOTIFY_MATCHERS.iter().any(|matcher| lowered == *matcher)
            || contains_ascii_token(&lowered, "spotify")
    })
}

fn contains_ascii_ci(value: &str, needle: &str) -> bool {
    value
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn contains_ascii_token(value: &str, token: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|candidate| candidate.eq_ignore_ascii_case(token))
}

fn eq_ascii_ci(value: Option<&str>, expected: Option<&str>) -> bool {
    match (value, expected) {
        (Some(value), Some(expected)) => value.eq_ignore_ascii_case(expected),
        (None, None) => true,
        _ => false,
    }
}

fn debug_log(message: &str) {
    if std::env::var_os("SPECTRUM_ARES_PIPEWIRE_DEBUG").is_some() {
        eprintln!("[spectrum-ares pipewire] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(object_type: &str, id: u32) -> DiscoveredSource {
        DiscoveredSource {
            id,
            object_serial: Some(u64::from(id + 100)),
            object_type: object_type.to_string(),
            client_id: None,
            linked_client_id: None,
            linked_client_name: None,
            node_name: None,
            node_description: None,
            application_name: None,
            application_process_binary: None,
            client_name: None,
            media_name: None,
            media_class: None,
            media_role: None,
            media_category: None,
            media_type: None,
            target_object: None,
            node_target: None,
        }
    }

    fn resolve(mut sources: Vec<DiscoveredSource>) -> Vec<DiscoveredSource> {
        resolve_spotify_links(&mut sources);
        sources
    }

    #[test]
    fn client_id_linking_selects_spotify_playback_node() {
        let mut spotify_client = source("Client", 135);
        spotify_client.application_name = Some("spotify".to_string());

        let mut playback_node = source("Node", 138);
        playback_node.client_id = Some(135);
        playback_node.node_name = Some("audio-src".to_string());
        playback_node.media_class = Some("Stream/Output/Audio".to_string());
        playback_node.media_category = Some("Playback".to_string());
        playback_node.media_role = Some("music".to_string());

        let sources = resolve(vec![spotify_client.clone(), playback_node.clone()]);
        let best = select_best_spotify_source(&sources).expect("expected Spotify candidate");

        assert_eq!(best.id, 138);
        assert_eq!(
            best.classification(),
            SourceClassification::LinkedSpotifyPlaybackNode
        );
        assert_eq!(best.linked_client_id, Some(135));
        assert!(best.is_capture_eligible());
    }

    #[test]
    fn spotify_client_alone_is_not_capture_eligible() {
        let mut spotify_client = source("Client", 135);
        spotify_client.application_name = Some("Spotify".to_string());

        let sources = resolve(vec![spotify_client]);
        let candidate = select_best_spotify_source(&sources).expect("expected Spotify client");

        assert_eq!(candidate.classification(), SourceClassification::ClientOnly);
        assert!(!candidate.is_capture_eligible());
    }

    #[test]
    fn generic_non_spotify_playback_node_is_not_selected() {
        let mut playback_node = source("Node", 201);
        playback_node.node_name = Some("audio-src".to_string());
        playback_node.media_class = Some("Stream/Output/Audio".to_string());
        playback_node.media_category = Some("Playback".to_string());
        playback_node.media_role = Some("music".to_string());

        let sources = resolve(vec![playback_node]);

        assert!(select_best_spotify_source(&sources).is_none());
    }

    #[test]
    fn direct_spotify_named_playback_node_still_works() {
        let mut spotify_node = source("Node", 301);
        spotify_node.node_name = Some("Spotify".to_string());
        spotify_node.media_class = Some("Stream/Output/Audio".to_string());
        spotify_node.media_category = Some("Playback".to_string());
        spotify_node.media_role = Some("music".to_string());

        let sources = resolve(vec![spotify_node]);
        let best = select_best_spotify_source(&sources).expect("expected Spotify playback node");

        assert_eq!(best.id, 301);
        assert_eq!(best.classification(), SourceClassification::Stream);
        assert!(best.is_capture_eligible());
    }

    #[test]
    fn linked_playback_node_scores_higher_than_client_only() {
        let mut spotify_client = source("Client", 135);
        spotify_client.application_name = Some("spotify".to_string());

        let mut playback_node = source("Node", 138);
        playback_node.client_id = Some(135);
        playback_node.node_name = Some("audio-src".to_string());
        playback_node.media_class = Some("Stream/Output/Audio".to_string());
        playback_node.media_category = Some("Playback".to_string());
        playback_node.media_role = Some("music".to_string());

        let sources = resolve(vec![spotify_client.clone(), playback_node.clone()]);
        let client = sources
            .iter()
            .find(|source| source.id == 135)
            .expect("Spotify client missing");
        let node = sources
            .iter()
            .find(|source| source.id == 138)
            .expect("Spotify playback node missing");

        assert!(node.spotify_candidate_score() > client.spotify_candidate_score());
    }

    #[test]
    fn spotify_matching_stays_case_insensitive() {
        let mut spotify_client = source("Client", 135);
        spotify_client.application_name = Some("com.spotify.Client".to_string());

        assert!(spotify_client.is_direct_spotify_match());
    }

    #[test]
    fn spotify_matching_does_not_trigger_on_unrelated_identifiers() {
        let mut helper_client = source("Client", 501);
        helper_client.application_name = Some("find_spotify_pipewire".to_string());

        assert!(!helper_client.is_direct_spotify_match());
    }

    #[test]
    fn audio_relevance_requires_audio_metadata() {
        let mut audio_node = source("Node", 601);
        audio_node.media_class = Some("Stream/Output/Audio".to_string());

        let mut generic_named_node = source("Node", 602);
        generic_named_node.node_name = Some("kwin_wayland".to_string());
        generic_named_node.media_class = Some("Stream/Output/Video".to_string());

        assert!(audio_node.is_audio_relevant());
        assert!(!generic_named_node.is_audio_relevant());
    }
}
