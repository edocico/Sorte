//! The list file format, schema 1. `fixtures/list-schema-1.json` is a document as this
//! version writes it: it must keep reading back to the same list, and the list must
//! keep writing back to the same JSON. If a test here fails after a model change, the
//! format changed: bump `SCHEMA_VERSION` and add a migration in `sorte-store` rather
//! than editing the fixture.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sorte_core::{
    AssetRef, DrawRecord, Entry, EntryId, ListDoc, ListId, ListSettings, ResultAction, Rgba8, SCHEMA_VERSION,
    SoundChoice, Sounds,
};
use uuid::Uuid;

const SCHEMA_1: &str = include_str!("fixtures/list-schema-1.json");

fn id(text: &str) -> EntryId {
    EntryId(Uuid::parse_str(text).unwrap())
}

fn instant(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn asset(sha256: &str, ext: &str) -> AssetRef {
    AssetRef::new(sha256, ext).unwrap()
}

/// The list the fixture describes, written out field by field.
fn expected() -> ListDoc {
    let pizza = id("0f6a1c2b-3d4e-4f50-8a1b-2c3d4e5f6a70");
    let sushi = id("1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d");
    let kebab = id("2b3c4d5e-6f70-4b8c-9dae-1f2a3b4c5d6e");
    let record = |at: &str, entry_id: EntryId, text: &str, picker: &str, ranking, action| DrawRecord {
        at: instant(at),
        entry_id,
        text: text.into(),
        picker: picker.into(),
        ranking,
        action,
    };
    ListDoc {
        schema: 1,
        id: ListId(Uuid::parse_str("4b7a3e0c-5d1f-4a2b-9c8e-1f2a3b4c5d6e").unwrap()),
        title: "Pranzo del venerdì".into(),
        description: "Dove si mangia questa settimana".into(),
        result_titles: vec!["Oggi si va da…".into(), "Il verdetto:".into()],
        picker: "wheel".into(),
        picker_settings: json!({ "speed": 6, "mystery": false }),
        settings: ListSettings {
            spin_duration_secs: 12,
            sounds: Sounds {
                start: SoundChoice::Default,
                ticks: SoundChoice::Off,
                end: SoundChoice::Custom(asset(
                    "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
                    "ogg",
                )),
            },
        },
        entries: vec![
            Entry { id: pizza, count: 2, ..Entry::new("Pizza") },
            Entry {
                id: sushi,
                label: Some("🍣 Sushi".into()),
                image: Some(asset("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "png")),
                weight: 3,
                color: Some(Rgba8 { r: 0xe4, g: 0x57, b: 0x2e, a: 0xff }),
                ..Entry::new("Sushi")
            },
            Entry {
                id: kebab,
                weight: 1000,
                color: Some(Rgba8 { r: 0x17, g: 0xbe, b: 0xbb, a: 0x80 }),
                enabled: false,
                ..Entry::new("Kebab")
            },
        ],
        history: vec![
            record("2026-11-05T12:15:42.123Z", pizza, "Pizza", "wheel", None, ResultAction::AddCount),
            record(
                "2026-11-05T12:16:03Z",
                id("3c4d5e6f-7081-4c9d-aebf-2a3b4c5d6e7f"),
                "Piadina",
                "wheel",
                None,
                ResultAction::HideChoice,
            ),
            record(
                "2026-11-05T12:20:00.000001Z",
                kebab,
                "Kebab",
                "horse-race",
                Some(vec![kebab, pizza, sushi]),
                ResultAction::HideAllInstances,
            ),
            record("2026-11-05T12:31:07.5Z", pizza, "Pizza", "wheel", None, ResultAction::Done),
        ],
        created_at: instant("2026-11-05T09:30:00Z"),
        updated_at: instant("2026-11-05T12:31:07.5Z"),
    }
}

#[test]
fn a_schema_1_document_reads_back_to_the_same_list() {
    let list: ListDoc = serde_json::from_str(SCHEMA_1).unwrap();
    assert_eq!(list.schema, SCHEMA_VERSION);
    assert_eq!(list.validate(), Ok(()));
    assert_eq!(list, expected());
}

#[test]
fn the_list_writes_back_to_the_same_json() {
    let written = serde_json::to_value(expected()).unwrap();
    let fixture: Value = serde_json::from_str(SCHEMA_1).unwrap();
    assert_eq!(written, fixture);
}
