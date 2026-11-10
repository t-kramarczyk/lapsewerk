# Änderungen

## 0.1.0 (Vorabversion, noch nicht veröffentlicht)

Erste vollständige Fassung, in dreißig Testrunden mit Amateur-, Hobby-, Profi-, Astro-, Postproduktions- und
Release-Prüfern geschliffen.

### App
- Projektverwaltung, sechs Schritte (Bilder → Analyse → Keyframes → Feinschliff → Vorschau → Export), Deutsch und
  Englisch, Einführung beim ersten Start und Hilfe zu jedem Schritt.
- Keyframes: Vorschläge an den Lichtwechseln, alle Regler von LightCraft, Masken (Verlauf, Radial, Himmel,
  Helligkeit, Pinsel), Übertragen, Rückgängig, „In LightCraft öffnen“ mit automatischem Neuladen.
- Auto-Übergang: nie bearbeitete Keyframes folgen den bearbeiteten; einer, der irgendwo
  bearbeitet wurde (App, LightCraft, von Hand), wird nie überschrieben. „Wieder automatisch“ für einzelne.
- Tag ↔ Nacht mit Anteil des echten Lichtwechsels (Vorgabe 15 %, im fertigen Look nachgemessen), Schwarz halten,
  Helligkeit von Hand formen (Holy-Grail-Kurve).
- Unbearbeitete Serien: Auto-Helligkeit (zu dunkler Tag auf Tageshelligkeit, Sternennacht mit dunklem Himmel) und
  ein Grund-Look, bis ein Keyframe bearbeitet ist.
- Zuschnitt aus der XMP wie in verbreiteten Zeitraffer-Programmen: wandert von Keyframe zu Keyframe, mittig auf die Videoform; Hochkant-
  Serien zeigen die gedrehten Maße schon auf den Format-Karten (dazu „Quer 4K“), Bild und Kurve nebeneinander.
- Deflicker in zwei Durchgängen mit selbstprüfendem Prüfdurchgang, Referenzbereich, regional (experimentell).
- Kamerafahrt (Zoom, Schwenk), Stabilisieren, Zwischenbilder, Entrauschen, Sensorflecken, Astro-Werkzeuge
  (Sterne abdunkeln, Lichtverschmutzung bis auf die dunkle Himmelsseite, Sternspuren nur im Himmel und in `--sky`,
  Warnung bei bewegter Kamera).
- Warnungen vor Blitzen, ausgebrannten Lichtern und Lücken, mit Reparatur (Ersetzen, Auffüllen).
- Export als Video (Mac; NTSC-Raten mit gleichmäßigem Bildtakt) oder Bildfolge (16-Bit-TIFF, 10-Bit-DPX, OpenEXR;
  HDR PQ/HLG), Timecode mit Drop-Frame in beiden (Timecode-Spur so lang wie der Film, Lieferzeile im Protokoll),
  fortsetzbar, mit INFO.txt (Lieferangaben, Reparaturen, alle Läufe, Befehl zum Nachrendern), SHA256SUMS.txt und
  HDR10-Werten.
- Render-Warteschlange; Schritt 6 sagt, was im Zielordner liegt (fertig, abgebrochen, andere Einstellungen mit
  „dort / hier“), und „Lieferung prüfen“ vergleicht jedes Bild mit seiner Prüfsumme; „Fortsetzen“ macht nur
  geänderte, gekürzte oder fehlende Bilder neu.
- Analyse und Ausgleich werden neben den Fotos gespeichert: Wiederöffnen ohne neue Messung.

### Kommandozeile `lapsewerk`
- `import`, `analyze` (mit `--write-keyframes`), `render` (alle Optionen der App, `--from-project`), `bench`,
  `calibrate` (Farbprofil der eigenen Kamera).
- App und Kommandozeile rechnen bitgleich und setzen gegenseitig fort.

### Sicherheit der Daten
- Originalfotos werden nie verändert. Eine Bildfolge geht nie in den Fotoordner; Dateien, die wie Bilder heißen,
  aber keine Bilddatei sind oder ohne Lapsewerks Manifest daneben liegen, werden nie überschrieben oder gelöscht.
- Abgeschnittene Raws (abgebrochene Kartenkopie) werden erkannt, ausgelassen und genannt, nicht als schwarzes Bild
  gerendert.
- Kaputte Fotos, Sidecars, Caches und Projektdateien führen zu einer Meldung, nie zu einem Absturz; eine kaputte
  `lapsewerk.json` bleibt als `lapsewerk.json.damaged` erhalten.

### Bekannt offen
- Videodateien (`.mov`) nur auf dem Mac; unter Linux die Bildfolge exportieren.
- Bleibende Lichtwechsel (Mondaufgang, Laternen) werden vom Deflicker etwas vorgezogen; die Handkurve hilft.
- Farbe weicht bei echten Raws von verbreiteten Raw-Entwicklern ab (A7 IV leicht lila, A6700 ohne Kamera-Matrix);
  In eigenen Messungen an zwei Serien zeigte Lapsewerk 3–4× weniger Restflackern als Filme derselben
  Raws aus einem anderen verbreiteten Zeitraffer-Ablauf (mit den damaligen Einstellungen).
