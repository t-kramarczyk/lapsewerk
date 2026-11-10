# Automatische Prüfung (GitHub Actions)

Bei jedem Pull Request (jedem neuen Stand darin) und jedem Push auf `main` prüft GitHub das Projekt
(`.github/workflows/ci.yml`). Ein neuer Stand bricht den laufenden Durchgang ab. Von Hand starten: Tab **Actions** → **CI** → **Run workflow**.

- **Linux:** Formatierung, Clippy ohne Warnungen, alle Tests, 100 000 Eingaben für den Fuzzer der Dateileser.
- **macOS:** alle Tests mit dem echten Encoder-Helfer (`encoder.swift`). Dabei entstehen echte ProRes-, HEVC- und
  HDR-Filme, und es wird geprüft, dass ein abgebrochener Helfer keinen Film hinterlässt. Danach rendert die Prüfung
  einmal komplett: SDR, HDR PQ, HDR HLG und Nachthimmel.

Das Ergebnis steht im Pull Request unten bei den Checks bzw. im Tab **Actions** des Repos.

## Kosten

Öffentliche Repos haben die Actions-Minuten frei. Ein
Lauf dauert auf macOS etwa 10–20 Minuten (der erste länger, danach hilft der Cache). Gestartet wird nur bei Pull Requests und auf `main`,
nicht bei jedem Push auf einen Arbeitszweig.
