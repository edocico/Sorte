//! Proptest strategies for the model, shared by the modules' tests. Ids come from the
//! strategy's own generator, not from `EntryId::random()`, so failures replay.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use proptest::collection::SizeRange;
use proptest::prelude::*;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    AssetRef, DrawRecord, Entry, EntryId, ListDoc, ListId, ListSettings, ResultAction, Rgba8, SCHEMA_VERSION,
    SPIN_DURATION_SECS, SoundChoice, Sounds, WEIGHT_RANGE,
};

pub(crate) fn entry_id() -> impl Strategy<Value = EntryId> {
    any::<u128>().prop_map(|n| EntryId(Uuid::from_u128(n)))
}

/// Any text, often one of a few that normalize alike, so that instances get matched.
pub(crate) fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec!["Anna Rossi", " anna  ROSSI", "Anna", "Bruno", "", "  "]).prop_map(str::to_owned),
        any::<String>(),
    ]
}

pub(crate) fn asset_ref() -> impl Strategy<Value = AssetRef> {
    ("[0-9a-f]{64}", "[a-z0-9]{1,5}").prop_map(|(sha256, ext)| AssetRef::new(&sha256, &ext).unwrap())
}

pub(crate) fn entry() -> impl Strategy<Value = Entry> {
    (
        entry_id(),
        text(),
        prop::option::weighted(0.2, text()),
        prop::option::weighted(0.1, asset_ref()),
        WEIGHT_RANGE,
        prop::option::weighted(0.2, any::<[u8; 4]>().prop_map(|[r, g, b, a]| Rgba8 { r, g, b, a })),
        prop::bool::weighted(0.8),
        prop_oneof![0u32..5, Just(u32::MAX)],
    )
        .prop_map(|(id, text, label, image, weight, color, enabled, count)| Entry {
            id,
            text,
            label,
            image,
            weight,
            color,
            enabled,
            count,
        })
}

/// Entries with distinct ids, as many as `size` asks. Of entries that share an id only the
/// first is kept, so there can be fewer (in practice only while shrinking), but never none
/// when `size` excludes 0.
pub(crate) fn entries(size: impl Into<SizeRange>) -> impl Strategy<Value = Vec<Entry>> {
    prop::collection::vec(entry(), size).prop_map(|mut entries| {
        let mut ids = HashSet::new();
        entries.retain(|entry| ids.insert(entry.id));
        entries
    })
}

pub(crate) fn picker() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["wheel", "horse-race"]).prop_map(str::to_owned)
}

/// Picker settings as pickers write them. Floats are left out: without its
/// `float_roundtrip` feature, serde_json may read one back a unit in the last place off.
pub(crate) fn picker_settings() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!({})),
        (1u8..=10, any::<bool>(), "[a-z ]{0,8}").prop_map(|(speed, mystery, theme)| {
            json!({ "speed": speed, "mystery": mystery, "theme": theme, "lanes": [2, 12] })
        }),
    ]
}

fn sound_choice() -> impl Strategy<Value = SoundChoice> {
    prop_oneof![Just(SoundChoice::Default), Just(SoundChoice::Off), asset_ref().prop_map(SoundChoice::Custom)]
}

pub(crate) fn settings() -> impl Strategy<Value = ListSettings> {
    (SPIN_DURATION_SECS, sound_choice(), sound_choice(), sound_choice()).prop_map(
        |(spin_duration_secs, start, ticks, end)| ListSettings {
            spin_duration_secs,
            sounds: Sounds { start, ticks, end },
        },
    )
}

pub(crate) fn action() -> impl Strategy<Value = ResultAction> {
    prop::sample::select(vec![
        ResultAction::Done,
        ResultAction::HideChoice,
        ResultAction::HideAllInstances,
        ResultAction::AddCount,
    ])
}

/// Any instant from 1970 to 2100, to the nanosecond.
fn instant() -> impl Strategy<Value = DateTime<Utc>> {
    (0i64..4_102_444_800, 0u32..1_000_000_000).prop_map(|(secs, nanos)| DateTime::from_timestamp(secs, nanos).unwrap())
}

fn draw_record() -> impl Strategy<Value = DrawRecord> {
    (instant(), entry_id(), text(), picker(), prop::option::of(prop::collection::vec(entry_id(), 0..4)), action())
        .prop_map(|(at, entry_id, text, picker, ranking, action)| DrawRecord {
            at,
            entry_id,
            text,
            picker,
            ranking,
            action,
        })
}

/// A valid list of up to 12 entries.
pub(crate) fn list() -> impl Strategy<Value = ListDoc> {
    list_with(0..=12)
}

/// A valid list with as many entries as `size` allows (see [`entries`]). Ask for a
/// non-empty list here rather than filter out empty ones with `prop_assume!`: proptest
/// gives up after too many rejected cases.
pub(crate) fn list_with(size: impl Into<SizeRange>) -> impl Strategy<Value = ListDoc> {
    (
        any::<u128>(),
        text(),
        text(),
        prop::collection::vec(text(), 0..3),
        picker(),
        picker_settings(),
        settings(),
        entries(size),
        prop::collection::vec(draw_record(), 0..4),
        instant(),
        instant(),
    )
        .prop_map(
            |(
                id,
                title,
                description,
                result_titles,
                picker,
                picker_settings,
                settings,
                entries,
                history,
                created_at,
                updated_at,
            )| {
                ListDoc {
                    schema: SCHEMA_VERSION,
                    id: ListId(Uuid::from_u128(id)),
                    title,
                    description,
                    result_titles,
                    picker,
                    picker_settings,
                    settings,
                    entries,
                    history,
                    created_at,
                    updated_at,
                }
            },
        )
}
