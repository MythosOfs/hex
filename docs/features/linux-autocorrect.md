# Autokorrektur und Textbausteine unter Hyprland

Aktiviere **Labs › Autokorrektur**. Solange der Schalter an ist, erscheint
**Autokorrektur** in der Seitenleiste. Hex korrigiert Deutsch und Englisch
beim Tippen in unterstützten Wayland-Textfeldern.

## Rückgängig mit Alt+Rechtsklick

Füge die Zeile zu deiner Hyprland-Konfiguration hinzu. Verwende den Namen
deines Fork-Binaries, hier `hex-autocorrect`.

`.config/hypr/hyprland.lua`:

```lua
hl.bind("ALT + mouse:273", hl.dsp.exec_cmd("hex-autocorrect autocorrect-undo"))
```

`.config/hypr/hyprland.conf`:

```ini
bind = ALT, mouse:273, exec, hex-autocorrect autocorrect-undo
```

Falls Alt+Rechtsklick bereits belegt ist, entferne die alte Belegung.
Der Shortcut wirkt auf das **aktive Textfeld**. Direkt nach einer
Korrektur bzw. einem Textbaustein stellt er die ursprüngliche Eingabe
inklusive Leerzeichen oder Satzzeichen wieder her. Tippe zum Testen
`so teh ` und drücke Alt+Rechtsklick, sobald `so the ` erscheint.

Mit **Backspace** direkt nach einer Autokorrektur übernimmst du die
ursprüngliche Schreibweise zusätzlich ins Wörterbuch. Unter
**Gelernte Wörter und Korrekturen** kannst du einzelne Wörter und
Korrekturpaare suchen und über **Vergessen** entfernen. Die **letzte
Korrektur** ist bis zum Beenden von Hex sichtbar und bleibt im
Arbeitsspeicher.

## Textbausteine

Schalte auf der Autokorrektur-Seite **Textbausteine einsetzen** ein.
Tippe das Kürzel am Feldanfang oder nach einem Leerzeichen und bestätige
es mit **Leertaste**. Das abschließende Leerzeichen bleibt erhalten.

| Kürzel | Ergebnis |
| --- | --- |
| `;datum` | Lokales Datum, z. B. `23.11.2026` |
| `;zeit` | Lokale Uhrzeit, z. B. `14:05` |
| `;lg` | Liebe Grüße |
| `;pfeil` | → |
| `;check` | ✓ |

## Apps ausschließen

Wähle auf der Autokorrektur-Seite unter **Ausgeschlossene Apps** die
Option **App hinzufügen**. Hex korrigiert, lernt und expandiert
in der ausgewählten App nichts. Der Schalter neben der App entfernt
sie aus der Ausnahmeliste und lässt die Funktionen in dieser App
wieder zu.

Der Desktop meldet Hex die aktive App. Wenn eine Ausnahmeliste aktiv
ist und die aktuelle App unbekannt bleibt, pausieren die Funktionen
für dieses Feld. Die Eingabemethode bleibt systemweit am Wayland-Seat
angemeldet, solange die Autokorrektur in Labs eingeschaltet ist.
