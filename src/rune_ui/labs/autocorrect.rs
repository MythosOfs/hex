use rune::{El, col, row, text};
use rune_kit::TextInput;

use super::super::controls::{Paint, note, page, quiet_button, section, section_with, toggle_row};
use super::super::modes::{self, PickerTarget};
use super::super::{Ui, platform_settings, scroll_body};
use crate::desktop::i18n::tr;
use crate::linux::autocorrect;

const PICKER: &str = "open-autocorrect-app-picker";

pub(in crate::rune_ui) fn pane(ui: &Ui, p: Paint, layer: &mut Vec<El>) -> El {
    let settings = &ui.settings.labs;
    if ui.modes.application_picker_open && ui.modes.picker_target == PickerTarget::Autocorrect {
        modes::application_picker(ui, p, &settings.autocorrect_excluded_apps, PICKER, layer);
    }
    let add = quiet_button(
        p,
        PICKER,
        tr("Add application"),
        tr("Add application"),
        |ui| {
            modes::toggle_picker(ui, PickerTarget::Autocorrect);
        },
    );
    let mut apps: Vec<_> = settings
        .autocorrect_excluded_apps
        .iter()
        .enumerate()
        .map(|(index, app)| {
            let name = app.clone();
            toggle_row(
                p,
                &format!("autocorrect-app-{index}"),
                app,
                None,
                false,
                move |ui| {
                    ui.update(|settings| {
                        settings
                            .labs
                            .autocorrect_excluded_apps
                            .retain(|app| app != &name);
                    });
                },
            )
        })
        .collect();
    if apps.is_empty() {
        apps.push(note("every-app", tr("Every app"), p.muted()).padding(12.0));
    }
    let snippets = section(
        p,
        "snippets",
        tr("Text snippets"),
        vec![toggle_row(
            p,
            "autocorrect-snippets",
            tr("Expand text snippets"),
            Some(";datum · ;zeit · ;lg · ;pfeil · ;check"),
            settings.autocorrect_snippets,
            |ui| {
                ui.update(|settings| settings.labs.autocorrect_snippets ^= true);
            },
        )],
    );
    let mut content = page("autocorrect-content")
        .children(platform_settings::error_section(ui, p))
        .child(section_with(
            p,
            "autocorrect-apps",
            tr("Disabled applications"),
            add,
            apps,
        ))
        .child(snippets);
    if ui.live()
        && let Some((original, fix)) = autocorrect::last_correction()
    {
        content = content.child(section(
            p,
            "last-correction",
            tr("Last correction"),
            vec![note("correction", &format!("{original} → {fix}"), p.text()).padding(12.0)],
        ));
    }
    let search = TextInput::new(&ui.autocorrect_search)
        .placeholder(tr("Search learned words and corrections"))
        .leading_icon(p.k.glyph.search)
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.autocorrect_search)
        .build(p.k)
        .key("autocorrect-search");
    let mut dictionary = col().key("learned-entries").gap(6.0).child(search);
    if ui.live() {
        for (is_typo, key, label) in autocorrect::entries(&ui.autocorrect_search.text) {
            dictionary = dictionary.child(
                row()
                    .key(format!("{is_typo}-{key}"))
                    .center_y()
                    .gap(8.0)
                    .child(
                        text(label.clone())
                            .key("word")
                            .font(p.k.ty.ui)
                            .color(p.text())
                            .grow(1.0),
                    )
                    .child(quiet_button(
                        p,
                        "forget",
                        tr("Forget"),
                        &format!("{} {label}", tr("Forget")),
                        move |ui| {
                            if let Err(error) = autocorrect::forget(is_typo, &key) {
                                ui.error = Some(error.to_string());
                            }
                        },
                    )),
            );
        }
    }
    content = content.child(
        col()
            .key("autocorrect-dictionary")
            .gap(6.0)
            .child(p.section_label(tr("Learned words and corrections")))
            .child(dictionary),
    );
    scroll_body(p.k, "autocorrect-scroll", content)
}
