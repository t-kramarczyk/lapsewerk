# Änderungen

## 0.3.0 (28.11.2026)

**Für Umsteiger von 0.2:** Bestehende Projekte bleiben ohne Nachschärfen und behalten ihren Tag ↔ Nacht-Wert; neue
Projekte schärfen leicht nach (0,3; von Tag bis Nacht zur Nacht hin auf 0,1 abgeschwächt, reine Nachtserien gar nicht)
und nehmen 10 %. Bildfolgen, die mit 0.2 begonnen wurden, lassen sich nicht mit 0.3 fortsetzen (anderes Programm,
andere Bilder): neu exportieren.

### App und Kommandozeile
- **Eigene Lieferung erkannt** auch nach automatischer Aufhellung; beiseitegelegte, bearbeitete Bilder (`.kept`) in
  einer fertigen Bildfolge nennt Schritt 6 gelb (nicht Teil der Lieferung).
- **Behoben: Holy Grail ohne Tag ↔ Nacht wurde schwarz.** Zeigen die Bilder die Belichtungsschritte der Kamera (etwa
  exportierte JPEGs), folgte das Video ohne `--keep-light` dem ganzen Lichtwechsel der Szene bis ins Schwarze. Jetzt
  gilt, was der Schalter verspricht: so hell, wie die Kamera belichtet hat, nur die Sprünge geglättet.
- **Sensorstaub wird gemeldet:** Ist „Sensorstaub entfernen“ aus, sucht die App beim ersten Berechnen der Vorschau
  trotzdem nach Flecken an immer derselben Stelle (wenige Sekunden; das Ergebnis bleibt bei den Korrekturen
  gespeichert). Findet sie welche, sagt die Vorschau es mit einem Knopf „Einschalten“; der Schalter in Schritt 4 und
  der Export-Schritt tragen den Hinweis, auch nach dem Wiederöffnen.
- **Kleinigkeiten aus den Tests:** Die Legende der Helligkeitskurve bricht in schmalen Fenstern um statt links
  abgeschnitten zu werden; starkes Flackern in Schritt 2 steht gelb statt rot da (es wird ausgeglichen, ziehende
  Wolken zählen mit); Schritt 3 erklärt die schon gesetzten Lichter und Tiefen des Grund-Looks.
- **Prüfurteil bei einem Blitz:** liegt in einem gemeldeten Abschnitt ein einzelnes Bild weit neben seinen Nachbarn,
  nennt die Prüfung dieses Bild und `--replace-outliers` statt einer langsamen Welle.
- **Probeaufnahmen weglassen:** Fotos am Anfang oder Ende, die durch eine lange Pause (mindestens 4 Intervalle und 30
  s) vom Rest getrennt sind, erkennt Lapsewerk als Probeaufnahmen. Die App bietet in Schritt 1 „Weglassen“ an (und
  „Doch verwenden“), die Kommandozeile nennt `--skip-first N` / `--skip-last N`. Gezeichnete Helligkeit und zum
  Ersetzen markierte Bilder wandern mit.
- **Automatische Aufhellung:** eine hell belichtete, beleuchtete Nacht (Lichtshow im Dunst) wurde wie ein zu dunkler
  Tag angehoben und wirkte milchig. Als Tag zählt jetzt nur, was laut Belichtung (EXIF, Lichtwert ab etwa 8–11; eine
  Dämmerung liegt darunter) bei Tageslicht entstand. In der App ist die Aufhellung sichtbar und abschaltbar (Schritt
  4).
- **Grund-Look:** nur noch Lichter −66 und Tiefen +80 (vorher dazu Kontrast +40 und Dynamik +15). Kontrast und Dynamik
  wirken in LightCraft je Farbkanal; ein aufgehellter Nachthimmel (Tag ↔ Nacht, Auto-Aufhellung) verlor damit den
  Rotkanal ganz – am Ende einer Stadt-Serie Tag → Nacht in über der Hälfte des Himmels (flaches Petrol, im Grade nicht
  zurückzuholen). Jetzt nirgends; dazu weniger übersättigt und mit offeneren Tiefen.
- Die Kommandozeile sagt bei `--auto-brightness`, welche Regel gegriffen hat (Nacht oder zu dunkler Tag).
- **Bearbeitete Keyframes legen die Helligkeit fest:** Mit zwei oder mehr bearbeiteten Keyframes (etwa aus einem
  anderen Raw-Programm) schaltet die App „Tag ↔ Nacht“ nicht mehr von selbst ein; Schritt 4 sagt es. Vorher wurde ein
  so bearbeiteter Sonnenaufgang am Anfang bis zu 1,9 Blenden dunkler als die Keyframes.
- App: Schritt 2 zeigt das Bild neben der Kurve (auch bei Querformat), die Legende erklärt die Keyframe-Rauten; die
  Fertig-Karte nach dem Export ohne doppelte Knöpfe; beschädigte Fotos am Serienrand stehen in Schritt 2 und in der
  Videolänge; lange Angaben in Schritt 1 brechen um; Export → Erweitert → Größe „Ganzes Foto“ (3:2 ohne Beschnitt);
  die Warteschlange schätzt die Restzeit nach dem Tempo der letzten Bilder.
- `import` sichert eine kaputte `lapsewerk.json` als `.damaged` (wie die App), statt sie zu ersetzen.
- Bildfolgen: Lieferzeile mit Größe und Bildzahl; HDR-EXR tragen `whiteLuminance` (203 cd/m²). Platzangaben unter 1 MB
  in KB.
- `--resume` vergleicht auch die Fotos (Anzahl, erstes, letztes): Ein Fortsetzen mit anderen weggelassenen
  Probeaufnahmen wird abgelehnt, statt zwei Bereiche zu mischen.
- XMP-Dateien, die schon beim Öffnen beschädigt sind, heißen nicht mehr „gerade beschädigt gespeichert“.
- **Objektivprofile:** Die Startseite bietet einmal an, die freie Objektiv-Datenbank lensfun (etwa 5 MB, CC BY-SA 3.0)
  herunterzuladen – nur auf Klick, nichts wird mitgeliefert, LightCraft nutzt dieselbe. Danach werden Objektive ohne
  eigene Korrektur in der Datei (z. B. Viltrox 25 mm an der a6400) bei der Verzeichnung ausgeglichen (die
  Randabdunklung bleibt, wie fotografiert). Raws, die eine eigene Verzeichnungskorrektur mitbringen und für die
  LightCraft sie anwendet (etwa E 11 mm an der ZV-E10), bleiben bei ihrer; Schritt 1 und das Log sagen, welche
  Korrektur gilt.
- LightCraft (öffentlich 61bce64): mit einem Objektivprofil aus der Datenbank wurde das Bild dunkler (der Abgleich mit
  dem Kamera-JPEG scheiterte); jetzt gleich hell wie ohne. a6100, a6600 und ZV-E10 bekommen die Farbmatrizen der a6400
  (gleicher Sensor): Weißabgleich in Kelvin statt eines Platzhalters.
- `INFO.txt` nennt Objektiv, Korrektur, Objektivprofile und den Grund-Look.
- Hinweis in App und Anleitung: Auch mit Keyframes aus einem anderen Raw-Programm bleiben die Farben die der Kamera.
- **Schärfer verkleinert, ohne schwarze Ringe:** LightCraft (öffentlich 61bce64) verkleinert jetzt mit einem
  Flächenmittel (Box). Vorher machte ein Filter mit negativen Ausläufern Pixel neben Stadtlichtern und hellen Planeten
  schwarz (Stadt bei Nacht: 204 Schwarzpixel in 10 Bildern, Ring um Jupiter); jetzt keine. Statt eines doppelt so
  breiten Zeltfilters: doppelt so viel Detail (eine Stadt-Serie in 540 × 960: 136 → 265, andere Zeitraffer-Programme
  255; ein See in 1620 × 1080: 237 → 511), fast doppelt so viele sichtbare Sterne, weiterhin keine dunklen Ringe an
  Lichtern, Flackern unverändert.
- **Objektivkorrektur ohne Weichzeichnen:** Mit Verzeichnungskorrektur (Objektivprofil) ging die Hälfte des
  Feindetails verloren, weil jedes Pixel zwischen zwei Pixeln der verkleinerten Zwischenstufe abgetastet wurde. Die
  Zwischenstufe liegt jetzt in doppelter Größe: eine Stadt-Serie in 540 × 960 hat 297 statt 237 (ohne Korrektur 397),
  ohne Moiré.
- **Video nachschärfen:** Ausgabeschärfung am fertigen Bild in Videogröße (App: an, Stärke 0,3, Regler bis 2;
  Kommandozeile `--sharpen F`). Nur Helligkeit, mit Schwelle gegen Rauschen und an den Nachbarpixeln begrenzt: keine
  dunklen Ringe um Sterne und Lichter, Flackern unverändert. Log und `INFO.txt` nennen sie.
- **Warnung bei Farben am Anschlag:** Der Export zählt, wo eine kräftige Farbe (meist ein satter Himmel) über mehr als
  10 % des Bildes an ihre Grenze stößt, und nennt je Abschnitt Bilder und Farbe samt Abhilfe (Sättigung in den
  Keyframes senken, fürs Grading HDR) – im Log (`colour clip:`), in der `INFO.txt` einer Bildfolge (`colour`) und in
  der Fertig-Anzeige der App. Nur SDR; weiße Lichter zählen nicht mit. `INFO.txt` nennt auch die Ausgabeschärfung
  (`sharpen`).
- Die Fertig-Anzeige nennt Farbe und Anteil („Bild 216–322 (Blau, bis 52 % des Bildes)“); ein Kanal auf 0 wird ab 15 %
  des Bildes gemeldet (nur dunkle, gefärbte Schatten; ein Abschnitt reicht so weit, wie seine Nachbarbilder knapp
  darunter liegen). Unter „Video nachschärfen“ erklärt die App bei Nachtserien, warum die Schärfung aus oder
  abgeschwächt ist (halb so viele feine dunkle Säume an Stadtlichtern und Sternen); bei einem neuen Projekt schärft
  die App eine Serie von Tag bis Nacht am Tag wie eingestellt, durch die erste Hälfte des Lichtwechsels voll, und
  schwächt sie in der zweiten zur Nacht hin auf ein Drittel ab (Kommandozeile `--sharpen-night 0.1`), eine reine
  Nachtserie (erkannt an der Belichtung, z. B. ein Sternenhimmel) gar nicht.
- Ein altes Projekt übernimmt die Schärfestärke seines letzten Exports.
- Fortsetzen mit einem anderen Programm- oder LightCraft-Stand wird abgelehnt („finish them with that one, or
  overwrite instead“); `INFO.txt` nennt beide in der Zeile `program`.
- **HEVC-Filme schärfer:** ohne eigene Bitrate richtet sich die Rate nach der Größe: Full HD 0,3 statt 0,2 Bit je
  Pixel (+7 % Detail, an einem Tagbild +13 %), 4K weiter 0,2, kleinere Filme bis 0,45 (540 × 960, bei 0,4 gemessen: 89
  % statt 83 % des Details eines halbierten 1080er-Films, wie ProRes). Die Dateien werden entsprechend größer. In der
  App gibt es unter Export → Erweitert die Qualität „Hoch“ (anderthalbfache Rate) für ein Master oder eine vorgegebene
  Rate.
- Abgelehntes Fortsetzen sagt jetzt auch: mit den Einstellungen von dort fortsetzen oder überschreiben.
- **Letztes Bild ohne Helligkeitssprung:** Wurde eine Szene am Ende von selbst ruhiger (eine Nacht erreicht), lief die
  Glättung mit dem früheren Gefälle weiter und das letzte Bild sprang (eine Stadt-Serie: −0,026 statt −0,01 Blenden).
  Am Rand folgt die Glättung jetzt einer engeren Linie, soweit der Unterschied über dem Rauschen der Serie liegt.
- **Fortsetzen mit automatischer Aufhellung:** wurde abgelehnt, wenn die gespeicherten Korrekturen wiederverwendet
  wurden (`brightness: … here none`); die Aufhellung wird jetzt mit ihnen gespeichert.
- **Tag ↔ Nacht:** Vorgabe der App 10 % statt 15 % des echten Lichtwechsels: die Nacht etwas heller, näher an anderen
  Zeitraffer-Programmen (bestehende Projekte behalten ihren Wert).
- **Randabdunklung ausgleichen:** neuer Haken in Schritt 4 (nur mit Objektivprofil aus der Datenbank), standardmäßig
  aus; Kommandozeile `--lens-vignette`. Aus bleibt nur die Verzeichnung korrigiert (in Nachtserien hellte die
  Vignettenkorrektur das Rauschen der Ecken mit auf).
- Wird der Video-Helfer von außen beendet (SIGKILL, meist Speichermangel), sagt die Meldung das und was hilft.

## 0.2.0 (22.11.2026)

### LightCraft auf dem Stand vom 22.11.2026
- Entwickelt mit dem aktuellen LightCraft (Rechenweg 10 statt 2): deutlich weniger Farbrauschen in Nachtserien,
  schonendere Lichter, bessere Lichter-/Tiefen-Regler ohne Blockartefakte, Korrekturen bei Objektiven und
  einzelnen Kameras, schnelleres Lesen von Fuji-Dateien. Neue Projekte und Keyframes aus `crs:`-Werten sehen
  dadurch sauberer und etwas satter aus als mit 0.1.0; Keyframes mit gespeicherten LightCraft-Einstellungen behalten
  ihren Rechenweg.
- Alle Keyframes einer Serie rechnen mit demselben Rechenweg (dem neuesten unter ihnen): kein Sprung an einem
  Keyframe, der aus einer anderen Quelle stammt.

### App
- Regler bleiben beim Scrollen ruhig (Bildlaufleiste immer sichtbar, feste Breite), Oberfläche immer dunkel.
- Restzeit aus dem Tempo der letzten 45 Sekunden statt aus dem Durchschnitt des ganzen Laufs.

## 0.1.0 (10.11.2026)

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
- Farbe weicht bei echten Raws von verbreiteten Raw-Entwicklern ab (A7 IV leicht lila, A6700 ohne Kamera-Matrix).

In eigenen Messungen an zwei Serien zeigte Lapsewerk 3–4× weniger Restflackern als Filme derselben Raws aus einem
anderen verbreiteten Zeitraffer-Ablauf (mit den damaligen Einstellungen).
