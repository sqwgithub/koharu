//! Texts, block translations, and attributes.

mod common;
use common::*;
use koharu_scene::{CharStyle, Content, Error, LayerId, Op, RichText, Session, Span, Text};
use serde_json::json;

fn text(session: &Session, layer: LayerId) -> Text {
    match &common::layer(session, layer).content {
        Content::Text(text) => (**text).clone(),
        Content::Block(block) => block.translation.clone().expect("a translation"),
        other => panic!("{other:?} holds no text"),
    }
}

fn set_text(session: &mut Session, layer: LayerId, text: &str) -> koharu_scene::Result<()> {
    session
        .apply(edit(vec![Op::SetText {
            layer: layer.into(),
            text: text.to_owned(),
        }]))
        .map(|_| ())
}

fn marked(text: &RichText) -> Vec<(&str, Option<u16>)> {
    text.0
        .iter()
        .map(|span| (span.insert.as_str(), span.attributes.weight))
        .collect()
}

#[test]
fn set_text_keeps_marks_on_the_characters_that_stayed() {
    let (_dir, mut session, page) = session();
    let layer = add(
        &mut session,
        page,
        None,
        json!({"kind": "text", "content": [
            {"insert": "Hello", "attributes": {"weight": 700}},
            {"insert": " world"},
        ]}),
    );
    set_text(&mut session, layer, "Hello there world").expect("a text");
    assert_eq!(
        marked(&text(&session, layer).content),
        [("Hello", Some(700)), (" there world", None)]
    );
    set_text(&mut session, layer, "Help world").expect("a text");
    assert_eq!(
        marked(&text(&session, layer).content),
        [("Hel", Some(700)), ("p world", None)]
    );

    // The rich editor's spans replace the text whole, joined as stored.
    session
        .apply(edit(vec![Op::SetRichText {
            layer: layer.into(),
            spans: vec![
                Span {
                    insert: "a".into(),
                    attributes: Default::default(),
                },
                Span {
                    insert: "".into(),
                    attributes: Default::default(),
                },
                Span {
                    insert: "b".into(),
                    attributes: Default::default(),
                },
            ],
        }]))
        .expect("spans");
    assert_eq!(marked(&text(&session, layer).content), [("ab", None)]);

    let raster = add(&mut session, page, None, json!({"kind": "raster"}));
    let error = set_text(&mut session, raster, "x").expect_err("a raster has no text");
    assert!(matches!(error, Error::Invalid { .. }), "{error}");
}

#[test]
fn typing_splices_and_marks_rich_text_keeping_the_other_characters_marks() {
    let bold = CharStyle {
        weight: Some(700),
        ..CharStyle::default()
    };
    let text = RichText(vec![
        Span {
            insert: "Hello".to_owned(),
            attributes: bold.clone(),
        },
        Span {
            insert: " world".to_owned(),
            attributes: CharStyle::default(),
        },
    ]);
    // A character typed at the bold word's end takes its marks.
    let typed = text.spliced(5..5, "!", &text.marks_at(5));
    assert_eq!(marked(&typed), [("Hello!", Some(700)), (" world", None)]);
    // Typing over a selection replaces it with the given marks.
    let over = text.spliced(3..8, "p wo", &CharStyle::default());
    assert_eq!(marked(&over), [("Hel", Some(700)), ("p world", None)]);
    // Marking a range splits its spans, and alike neighbours join again.
    let light = CharStyle {
        weight: Some(300),
        ..CharStyle::default()
    };
    let lightened = text.marked(3..8, &light);
    assert_eq!(
        marked(&lightened),
        [("Hel", Some(700)), ("lo wo", Some(300)), ("rld", None)]
    );
    assert_eq!(
        marked(&lightened.marked(3..5, &bold)),
        [("Hello", Some(700)), (" wo", Some(300)), ("rld", None)]
    );
    // At the start a character takes the first one's marks.
    assert_eq!(text.marks_at(0), bold);
    assert_eq!(text.marks_at(6), CharStyle::default());
    assert_eq!(RichText::default().marks_at(0), CharStyle::default());
}

#[test]
fn text_and_duplicates_survive_reopening() {
    let (_dir, path) = path();
    let mut session = Session::create(&path).expect("a project");
    let page = page(&mut session);
    let balloon = add(
        &mut session,
        page,
        None,
        json!({"kind": "balloon", "outline": outline()}),
    );
    let block = add(&mut session, page, Some(balloon), block(0.0));
    set_text(&mut session, block, "Hi").expect("a translation");
    let change = session
        .apply(edit(vec![Op::DuplicateLayer {
            layer: balloon.into(),
            id: None,
            index: None,
        }]))
        .expect("a duplicate");
    let (copy, copied_block) = (created_layer(&change, 0), created_layer(&change, 1));
    assert_eq!(layer(&session, copy).name, layer(&session, balloon).name);
    assert_eq!(text(&session, copied_block).content.text(), "Hi");
    set_text(&mut session, copied_block, "Bye").expect("a translation");
    assert_eq!(
        text(&session, block).content.text(),
        "Hi",
        "the copy is its own"
    );

    let before = session.project();
    drop(session);
    let session = Session::open(&path).expect("it opens");
    assert_eq!(session.project(), before);
    assert_eq!(text(&session, copied_block).content.text(), "Bye");
}

#[test]
fn a_blocks_translation_comes_and_goes_with_its_own_ops() {
    let (_dir, mut session, page) = session();
    let block = add(&mut session, page, None, block(0.0));
    set_text(&mut session, block, "Hello").expect("a translation");
    assert_eq!(
        text(&session, block),
        Text {
            content: RichText::from("Hello"),
            ..Text::default()
        }
    );
    apply(
        &mut session,
        vec![json!({"op": "update_layer", "layer": block, "patch": {"translation": null}})],
    )
    .expect("no translation");
    let Content::Block(gone) = &layer(&session, block).content else {
        panic!("not a block")
    };
    assert_eq!(gone.translation, None);
    session.undo().expect("undo").expect("a step");
    assert_eq!(text(&session, block).content.text(), "Hello");

    let balloon = add(
        &mut session,
        page,
        None,
        json!({"kind": "balloon", "outline": outline()}),
    );
    let error = set_text(&mut session, balloon, "x").expect_err("a balloon has no text");
    assert!(matches!(error, Error::Invalid { .. }), "{error}");

    // Attributes: the last writer wins, and an empty namespace goes.
    let set = |value: serde_json::Value| json!({"op": "set_attribute", "layer": block, "namespace": "c2pa", "key": "ocr", "value": value});
    apply(&mut session, vec![set(json!({"action": "c2pa.created"}))]).expect("an attribute");
    apply(&mut session, vec![set(json!({"action": "c2pa.edited"}))]).expect("an attribute");
    assert_eq!(
        layer(&session, block).attributes["c2pa"]["ocr"],
        json!({"action": "c2pa.edited"})
    );
    apply(&mut session, vec![set(serde_json::Value::Null)]).expect("no attribute");
    assert!(layer(&session, block).attributes.is_empty());

    // A lock above stops text and attribute writes alike.
    apply(
        &mut session,
        vec![json!({"op": "update_layer", "layer": block, "patch": {"locked": true}})],
    )
    .expect("a lock");
    let error = set_text(&mut session, block, "x").expect_err("locked");
    assert!(matches!(error, Error::Locked { .. }), "{error}");
    let error = apply(&mut session, vec![set(json!(1))]).expect_err("locked");
    assert!(matches!(error, Error::Locked { .. }), "{error}");
}

#[test]
fn apply_candidate_preserves_marks_exclusions_and_undo() -> koharu_scene::Result<()> {
    use koharu_scene::{GlossaryEntry, GlossaryKind};
    let (_dir, mut session, page) = session();
    let source = "お兄さん";
    let add_line = |session: &mut Session, source: &str, translated: serde_json::Value| {
        add(
            session,
            page,
            None,
            json!({"kind":"block",
            "source":{"content": source}, "translation":{"content": translated}}),
        )
    };
    let changed = add_line(
        &mut session,
        "お兄さんだ",
        json!([
            {"insert":"是","attributes":{"weight":300}},
            {"insert":"哥","attributes":{"weight":700}},
            {"insert":"哥"},
            {"insert":"！","attributes":{"weight":400}},
        ]),
    );
    let excluded = add_line(&mut session, source, json!("哥哥"));
    let collision = add_line(&mut session, "お兄さんも弟も", json!("哥哥和弟弟"));
    let already = add_line(&mut session, source, json!("是大哥哥。"));
    let unknown = add_line(&mut session, source, json!("是家里的老大。"));
    let ambiguous = add_line(&mut session, source, json!("哥哥还是哥哥？"));
    let stale = add_line(&mut session, "娘", json!("哥哥"));
    let manual_name = add_line(&mut session, source, json!("原来是兄长。"));
    let locked = add_line(&mut session, source, json!("哥哥"));
    let hidden_page = common::page(&mut session);
    let on_hidden_page = add(
        &mut session,
        hidden_page,
        None,
        json!({"kind":"block","source":{"content":source},"translation":{"content":"哥哥"}}),
    );
    apply(
        &mut session,
        vec![
            json!({"op":"update_layer","layer":locked,"patch":{"locked":true}}),
            json!({"op":"set_page_options","page":hidden_page,"excluded":true}),
        ],
    )?;
    let mut current = GlossaryEntry {
        source: source.into(),
        translation: "大哥哥".into(),
        kind: GlossaryKind::Term,
        note: String::new(),
        wiki_url: None,
        excluded_layers: [excluded].into(),
    };
    let old = GlossaryEntry {
        translation: "哥哥".into(),
        ..current.clone()
    };
    let other = GlossaryEntry {
        source: "弟".into(),
        ..old.clone()
    };
    let ops = session.project().analyzed_glossary(
        vec![vec![old.clone(), current.clone()]],
        vec![current.clone(), other.clone()],
        vec![],
    );
    session.apply(common::edit(ops))?;
    session.apply(common::edit(vec![Op::EditManualCandidate {
        source: source.into(),
        translation: "兄长".into(),
        remove: false,
    }]))?;
    let locations = [
        manual_name,
        locked,
        on_hidden_page,
        changed,
        changed,
        excluded,
        collision,
        already,
        unknown,
        ambiguous,
        stale,
    ];
    let ops = session
        .project()
        .apply_candidate(&current, &locations, &[])?;
    assert_eq!(ops.len(), 2);
    session.apply(common::edit(ops))?;
    assert_eq!(text(&session, manual_name).content.text(), "原来是大哥哥。");
    assert_eq!(
        marked(&text(&session, changed).content),
        [("是", Some(300)), ("大哥哥", Some(700)), ("！", Some(400))]
    );
    assert!(
        session
            .project()
            .apply_candidate(&current, &locations, &[])?
            .is_empty()
    );
    session.undo()?;
    assert_eq!(
        marked(&text(&session, changed).content),
        [
            ("是", Some(300)),
            ("哥", Some(700)),
            ("哥", None),
            ("！", Some(400))
        ]
    );

    // A generated alternative which contains the new spelling can be shortened.
    let previous = session.project().glossary().to_vec();
    current.translation = old.translation;
    session.apply(common::edit(vec![Op::SetGlossary {
        previous,
        entries: vec![current.clone(), other],
    }]))?;
    let ops = session
        .project()
        .apply_candidate(&current, &[already], &[])?;
    assert_eq!(ops.len(), 1);
    session.apply(common::edit(ops))?;
    assert_eq!(text(&session, already).content.text(), "是哥哥。");
    Ok(())
}
