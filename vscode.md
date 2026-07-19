# Loopbiotic for VS Code

## Cel

Rozszerzenie VS Code powinno zachować obecny sposób pracy Loopbiotic, a nie tworzyć osobny harness. Logika sesji, walidacja odpowiedzi, patch gate, pamięć celu i prefetch pozostają w `loopbioticd`. Rozszerzenie jest drugim klientem tego samego protokołu, obok integracji Neovim.

Docelowy przepływ:

```text
prompt
  → hypothesis
  → finding
  → decyzja użytkownika: follow / why / fix
  → jeden zwalidowany lokalny patch
  → review
  → accept / reject
  → następny krok
```

Najważniejsze własności:

- agent nie modyfikuje kodu przed świadomą decyzją użytkownika,
- jedna karta opisuje jeden krok,
- `follow`, `why` i `fix` pozostają osobnymi przejściami stanu,
- jeden patch obejmuje mały, lokalny i możliwy do sprawdzenia fragment,
- patch musi przejść walidację przed udostępnieniem akcji Apply,
- zaakceptowanie patcha nie autoryzuje pozostałych zmian,
- kolejny krok może być przygotowywany w tle podczas review bieżącej karty,
- conversation i patch korzystają z oddzielnych, ciepłych lanes modelu.

## Architektura

```text
                         ┌─ Neovim UI
model ↔ loopbioticd ↔ JSON-RPC
                         └─ VS Code extension
                              ├─ Webview View: karty i streaming
                              ├─ native diff editor: review patcha
                              └─ VS Code commands: akcje użytkownika
```

### `loopbioticd`

Daemon pozostaje źródłem prawdy dla:

- maszyny stanów sesji,
- kontraktów kart,
- optymalizacji kontekstu,
- rozmowy z backendem,
- streamingu provisional preview,
- normalizacji i walidacji patchy,
- retry oraz repair,
- pamięci celu i ukończonych kroków,
- prefetchu kolejnego kroku.

### Rozszerzenie VS Code

Cienka warstwa TypeScript:

1. Uruchamia `loopbioticd --stdio` jako trwały proces.
2. Wykonuje handshake protokołu.
3. Wysyła kontekst aktywnego edytora przez JSON-RPC.
4. Renderuje karty i zdarzenia progress.
5. Rejestruje komendy odpowiadające akcjom kart.
6. Pokazuje zwalidowany patch w natywnym diff editorze.
7. Wysyła wynik accept/reject z aktualnym stanem dokumentu.

Rozszerzenie nie powinno implementować ponownie logiki przejść ani walidacji patcha.

## Interfejs

### Webview View

Widok w bocznym panelu lub Secondary Side Bar renderuje jedną aktualną kartę. Powinien obsługiwać stany:

```text
Thinking → Streaming draft → Validating → Review → Result
```

Provisional preview może pokazywać wyłącznie bezpieczne pola opisowe:

- `title`,
- `claim`,
- `finding`,
- `question`,
- `explanation`,
- `reason`,
- `summary`.

Nie wolno udostępniać Apply ani wykonywać zmian podczas streamingu. Akcje finalnej karty pojawiają się dopiero po odebraniu kompletnej odpowiedzi i przejściu patch gate.

Webview powinien zachowywać focus edytora, obsługiwać klawiaturę i aktualizować istniejącą kartę zamiast dopisywać kolejne kopie częściowej odpowiedzi.

### Review patcha

Zwalidowany patch jest prezentowany przez natywny diff editor VS Code:

- lewa strona: bieżący dokument,
- prawa strona: wirtualny dokument z propozycją,
- Accept: zastosowanie kontrolowanego `WorkspaceEdit`,
- Reject: odrzucenie bez modyfikacji pliku,
- Retry: nowa próba dla tego samego kroku,
- Why: powrót do wyjaśnienia bez utraty pending patcha.

Przed Apply rozszerzenie ponownie sprawdza wersję dokumentu. Drift powoduje odmowę zastosowania i powrót do retry/repair zamiast cichego dopasowania zmiany do innej treści.

## Kontekst VS Code

Do `ContextBundle` należy mapować:

- URI i ścieżkę aktywnego dokumentu,
- pozycję kursora,
- zaznaczenie,
- widoczny lub ograniczony fragment bufora,
- niezapisany tekst dokumentu,
- diagnostykę z `vscode.languages.getDiagnostics`,
- definicje, deklaracje, implementacje i referencje przez commands/LSP,
- symbole workspace z krótkim deadline,
- aktualną wersję dokumentu do kontroli driftu.

Zbieranie kosztowniejszych wskazówek powinno mieć twardy budżet czasu. Request do modelu nie może czekać bez ograniczenia na provider symboli lub serwer językowy.

## Latency i telemetry

Rozszerzenie powinno rejestrować monotoniczne punkty pomiarowe:

```text
submit
  → context ready
  → daemon request
  → provider request
  → first delta
  → first provisional preview
  → complete response
  → validated card
  → rendered card
```

Treść promptów, preview i patchy pozostaje redagowana w logach. Widoczne mogą być typ zdarzenia, faza, czas, liczba bajtów, hash i metryki tokenów.

## Kolejność wdrożenia

1. Minimalne rozszerzenie TypeScript i lifecycle procesu `loopbioticd`.
2. Handshake oraz obsługa request/response/notification JSON-RPC.
3. Przechwytywanie kontekstu aktywnego edytora.
4. Webview z kartami hypothesis, finding, choice, working i summary.
5. Streaming provisional preview bez aktywnych akcji finalnych.
6. Natywny diff editor dla jednej zwalidowanej propozycji.
7. Accept/reject/retry/why/follow/fix jako komendy VS Code.
8. Kontrola driftu i bezpieczny `WorkspaceEdit`.
9. Prefetch oraz odtwarzanie sesji po ponownym otwarciu widoku.
10. Testy integracyjne rozszerzenie ↔ daemon i pomiary TTFT.

## Kryteria ukończenia

Integracja jest gotowa, gdy:

- ten sam scenariusz daje równoważne przejścia stanów w Neovim i VS Code,
- first preview pojawia się bez oczekiwania na finalny JSON,
- żadna akcja zmieniająca plik nie jest dostępna przed walidacją,
- jeden Accept stosuje tylko jeden zatwierdzony krok,
- anulowanie zatrzymuje aktywną turę backendu,
- drift dokumentu jest wykrywany przed Apply,
- restart widoku nie zabija niepotrzebnie ciepłego daemona,
- logi pozwalają zmierzyć TTFT bez ujawniania kodu użytkownika.
