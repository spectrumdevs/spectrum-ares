#[path = "../src/backend/pipewire/discovery.rs"]
mod pipewire_discovery;

use pipewire_discovery::{DiscoveredSource, SourceClassification};

fn main() {
    match pipewire_discovery::discover_sources() {
        Ok(sources) => {
            let spotify_clients: Vec<&DiscoveredSource> = sources
                .iter()
                .filter(|source| source.object_type == "Client" && source.is_direct_spotify_match())
                .collect();
            let playback_nodes: Vec<&DiscoveredSource> = sources
                .iter()
                .filter(|source| source.looks_like_playback_node())
                .collect();
            let linked_candidates: Vec<&DiscoveredSource> = sources
                .iter()
                .filter(|source| {
                    source.classification() == SourceClassification::LinkedSpotifyPlaybackNode
                })
                .collect();
            let best = pipewire_discovery::select_best_spotify_source(&sources);

            print_section("Spotify-like clients", &spotify_clients);
            print_section("Playback/audio nodes", &playback_nodes);
            print_section("Linked Spotify playback candidates", &linked_candidates);

            println!("Best candidate");
            println!("--------------");
            match best {
                Some(source) => {
                    print_source(&source);
                    println!("selection: {}", source.selection_reason());
                }
                None => {
                    if !spotify_clients.is_empty() {
                        println!("No capture-eligible Spotify playback stream/node was found.");
                        println!(
                            "Spotify may need to be actively playing, or PipeWire may expose the stream differently via pipewire-pulse."
                        );
                    } else {
                        println!("No likely Spotify clients were found.");
                    }
                }
            }
        }
        Err(err) => {
            eprintln!("PipeWire discovery failed: {err}");
            std::process::exit(1);
        }
    }
}

fn print_section(title: &str, sources: &[&DiscoveredSource]) {
    println!("{title}");
    println!("{}", "-".repeat(title.len()));

    if sources.is_empty() {
        println!("(none)");
        println!();
        return;
    }

    for source in sources {
        print_source(source);
        println!();
    }
}

fn print_source(source: &DiscoveredSource) {
    println!(
        "#{:<4} serial={:<6} type={:<14} class={:<26} score={:<4} eligible={} {}",
        source.id,
        format_optional_u64(source.object_serial),
        source.object_type,
        source.classification(),
        source.spotify_candidate_score(),
        yes_no(source.is_capture_eligible()),
        source.display_name()
    );
    print_optional("node.name", source.node_name.as_deref());
    print_optional("node.description", source.node_description.as_deref());
    print_optional("application.name", source.application_name.as_deref());
    print_optional(
        "application.process.binary",
        source.application_process_binary.as_deref(),
    );
    print_optional("client.name", source.client_name.as_deref());
    print_optional("media.name", source.media_name.as_deref());
    print_optional("media.class", source.media_class.as_deref());
    print_optional("media.category", source.media_category.as_deref());
    print_optional("media.role", source.media_role.as_deref());
    print_optional("media.type", source.media_type.as_deref());
    print_optional_u32("client.id", source.client_id);
    print_optional_u32("linked_client_id", source.linked_client_id);
    print_optional("linked_client_name", source.linked_client_name.as_deref());
    print_optional("target.object", source.target_object.as_deref());
    print_optional("node.target", source.node_target.as_deref());
    println!(
        "  direct_spotify_match: {}",
        yes_no(source.is_direct_spotify_match())
    );
    println!("  spotify_related: {}", yes_no(source.is_spotify_related()));
    println!(
        "  capture_eligible: {}",
        yes_no(source.is_capture_eligible())
    );
    println!("  reason: {}", source.capture_eligibility_reason());
}

fn print_optional(label: &str, value: Option<&str>) {
    if let Some(value) = value {
        println!("  {label}: {value}");
    }
}

fn print_optional_u32(label: &str, value: Option<u32>) {
    if let Some(value) = value {
        println!("  {label}: {value}");
    }
}

fn format_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
