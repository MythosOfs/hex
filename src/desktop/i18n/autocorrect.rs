//! Labs › Autocorrect's strings in the languages whose tables live beside
//! the main one, so those stay under the source cap.
//!
//! Not yet reviewed by native speakers, like their neighbouring tables.

#[rustfmt::skip]
pub(super) const FRENCH: &[(&str, &str)] = &[
    ("This desktop doesn't offer the input method Autocorrect needs (GNOME doesn't)", "Ce bureau n'offre pas la méthode de saisie dont la Correction automatique a besoin (GNOME non plus)"),
    ("Smarter autocorrect", "Correction automatique plus fine"),
    ("With Autocorrect on, a small language model reads the sentence to choose fixes, and catches real words typed for others. Downloads Gemma 3 270M (290 MB) and needs llama.cpp installed", "Avec la Correction automatique activée, un petit modèle de langue lit la phrase pour choisir les corrections et repère les vrais mots tapés à la place d'autres. Télécharge Gemma 3 270M (290 Mo) et nécessite llama.cpp"),
    ("Autocorrect", "Correction automatique"),
    ("On Wayland, fix clear typos as you finish each word, in German and English at once. Backspace right after a fix undoes it, and Hex learns the word", "Sous Wayland, corrige les fautes de frappe évidentes à la fin de chaque mot, en allemand et en anglais à la fois. Retour arrière juste après une correction l'annule, et Hex apprend le mot"),
];

#[rustfmt::skip]
pub(super) const PORTUGUESE: &[(&str, &str)] = &[
    ("This desktop doesn't offer the input method Autocorrect needs (GNOME doesn't)", "Esta área de trabalho não oferece o método de entrada de que a Correção automática precisa (o GNOME não oferece)"),
    ("Smarter autocorrect", "Correção automática mais esperta"),
    ("With Autocorrect on, a small language model reads the sentence to choose fixes, and catches real words typed for others. Downloads Gemma 3 270M (290 MB) and needs llama.cpp installed", "Com a Correção automática ativada, um pequeno modelo de linguagem lê a frase para escolher as correções e detecta palavras reais digitadas no lugar de outras. Baixa o Gemma 3 270M (290 MB) e precisa do llama.cpp instalado"),
    ("Autocorrect", "Correção automática"),
    ("On Wayland, fix clear typos as you finish each word, in German and English at once. Backspace right after a fix undoes it, and Hex learns the word", "No Wayland, corrige erros de digitação óbvios ao terminar cada palavra, em alemão e inglês ao mesmo tempo. Backspace logo após uma correção a desfaz, e o Hex aprende a palavra"),
];

#[rustfmt::skip]
pub(super) const RUSSIAN: &[(&str, &str)] = &[
    ("This desktop doesn't offer the input method Autocorrect needs (GNOME doesn't)", "Этот рабочий стол не предоставляет метод ввода, нужный автоисправлению (GNOME его не предоставляет)"),
    ("Smarter autocorrect", "Более умное автоисправление"),
    ("With Autocorrect on, a small language model reads the sentence to choose fixes, and catches real words typed for others. Downloads Gemma 3 270M (290 MB) and needs llama.cpp installed", "Когда автоисправление включено, небольшая языковая модель читает предложение, чтобы выбрать исправления, и замечает настоящие слова, набранные вместо других. Скачивает Gemma 3 270M (290 МБ) и требует установленного llama.cpp"),
    ("Autocorrect", "Автоисправление"),
    ("On Wayland, fix clear typos as you finish each word, in German and English at once. Backspace right after a fix undoes it, and Hex learns the word", "В Wayland исправляет явные опечатки в конце каждого слова, на немецком и английском одновременно. Backspace сразу после исправления отменяет его, и Hex запоминает слово"),
];
