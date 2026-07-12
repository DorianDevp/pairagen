# Klasyczny ML dla optymalizacji pracy Pair

## Status

Ten dokument opisuje opcjonalny kierunek rozwoju na przyszłość. Bieżący system
optymalizacji kontekstu ma pozostać w pełni deterministyczny i nie zależy od
modelu ML, procesu treningowego ani środowiska Python.

## Cel

Najbardziej uzasadnionym zastosowaniem klasycznego ML w Pair nie jest
generowanie lub rozumienie kodu. Jest nim ranking kontekstu i wybór polityki
wykonania: dostarczenie agentowi tych fragmentów repozytorium, które mają
największą oczekiwaną użyteczność przy możliwie małym koszcie tokenowym i
czasowym.

Model powinien być opcjonalną, wymienną funkcją rankującą nad kandydatami
utworzonymi przez deterministyczny indeks i graf zależności. Awaria, brak modelu
albo zbyt mała ilość danych muszą automatycznie pozostawiać system przy rankingu
heurystycznym.

## Główny przypadek użycia: ranking fragmentów kontekstu

Przepływ docelowy:

```text
prompt + kursor + zaznaczenie + diagnostyki
                    ↓
      deterministyczny generator kandydatów
                    ↓
          graf symboli i zależności
                    ↓
      heurystyka lub opcjonalny model ML
                    ↓
        pakowanie do budżetu tokenów
                    ↓
                  agent
```

Dla każdego kandydata model przewiduje użyteczność fragmentu w danym zadaniu.
Ranking powinien uwzględniać koszt:

```text
wartość(kandydat) = P(użyteczny | zadanie, kandydat) - λ * koszt_tokenów
```

Lepszym celem od samego prawdopodobieństwa użycia może być oczekiwany przyrost
szansy na zaakceptowany krok na jeden token.

## Przykładowe cechy

Cechy powinny być tanie, stabilne i w większości niezależne od konkretnego
języka:

- odległość w grafie wywołań lub importów,
- odległość katalogu od aktualnego pliku,
- definicja albo użycie symbolu spod kursora,
- pokrycie nazw z promptu przez nazwy symboli i ścieżkę,
- powiązanie z aktywną diagnostyką,
- rodzaj symbolu i fragmentu,
- liczba referencji do symbolu,
- informacja, czy fragment jest testem,
- historyczne współzmienianie plików,
- liczba tokenów fragmentu,
- świeżość wpisu w indeksie,
- dotychczasowa skuteczność podobnych kandydatów,
- tryb sesji i rodzaj oczekiwanej karty,
- liczba wcześniejszych retry i odrzuceń.

Nie należy rozpoczynać od surowego kodu, embeddingów ani dużej liczby cech
tekstowych. Model ma poprawiać selekcję dokonaną przez analizator, a nie zastępować
analizę kodu.

## Etykiety z interakcji

Pair ma naturalną pętlę informacji zwrotnej. Etykiety mogą być wyprowadzane bez
ręcznego oznaczania danych.

Sygnały pozytywne:

- fragment znalazł się w zaakceptowanym patchu,
- wskazana lokalizacja została otwarta przez użytkownika,
- użytkownik wybrał `Follow`,
- diagnostyka zniknęła po zastosowaniu patcha,
- krok został zaakceptowany bez ręcznej korekty,
- sesja osiągnęła cel bez retry.

Sygnały negatywne:

- patch został odrzucony,
- użytkownik wybrał `Other` albo `Retry`,
- dostarczony fragment nie został wykorzystany,
- agent musiał samodzielnie odnaleźć pominięty plik,
- kontrakt patcha wymagał naprawy,
- po zmianie pojawiły się nowe diagnostyki lub niepowodzenia testów.

Najmocniejszą etykietą jest zaakceptowanie patcha bez edycji. Samo zaakceptowanie
nie wystarcza, ponieważ użytkownik może wcześniej istotnie zmienić draft.

## Telemetria potrzebna do przyszłego treningu

Dla każdej sesji i każdego rozważanego kandydata warto zapisywać:

```json
{
  "session_id": "s_123",
  "repository_revision": "abc123",
  "candidate_id": "src/users/email.rs::UserEmail::parse",
  "features": {
    "graph_distance": 1,
    "is_definition": true,
    "is_test": false,
    "prompt_name_overlap": 0.67,
    "estimated_tokens": 184
  },
  "heuristic_score": 8.4,
  "selected": true,
  "later_used": true,
  "patch_accepted": true,
  "patch_edit_distance": 0
}
```

Dodatkowo na poziomie kroku:

- hash zaproponowanego i ostatecznie zastosowanego patcha,
- dystans edycyjny pomiędzy nimi,
- czas do akceptacji lub odrzucenia,
- diagnostyki przed i po,
- testy przed i po,
- tokeny wejściowe i wyjściowe,
- czas backendu,
- liczbę automatycznych retry,
- wybrany backend, model i effort.

Kod źródłowy i prompty mogą zawierać dane prywatne. Dataset treningowy powinien
domyślnie pozostawać lokalny, umożliwiać wyłączenie zapisu treści i przechowywać
wersję zanonimizowanych cech zamiast pełnych fragmentów.

## Proponowane modele

Kolejność eksperymentów:

1. Regresja logistyczna jako interpretowalny baseline.
2. Gradient boosting, np. LightGBM albo XGBoost.
3. Learning-to-rank, np. LambdaMART, gdy dostępna będzie odpowiednia liczba
   całych sesji.
4. Contextual bandit dopiero dla kontrolowanego wyboru backendu, modelu lub
   budżetu.

Model należy oceniać względem rankingu heurystycznego, a nie tylko względem
losowego wyboru.

## Dalsze zastosowania

Po rankingu kontekstu te same dane mogą wspierać:

- wybór rozmiaru lokalnego wycinka kodu,
- wybór backendu, modelu i effort,
- przewidywanie ryzyka kosztownego retry,
- ranking testów do uruchomienia w ograniczonym budżecie czasu,
- wykrywanie utknięcia sesji,
- prognozę liczby tokenów i czasu kroku.

Deterministyczne walidatory zawsze pozostają źródłem prawdy. ML może dobrać
strategię przed wywołaniem backendu, ale nie powinien zatwierdzać patcha ani
zastępować kontroli kontraktu.

## Trening i ewaluacja

Danych nie wolno losowo dzielić po pojedynczych kandydatach. Kandydaci z jednej
sesji są silnie skorelowani i spowodowaliby leakage. Podział powinien następować
po całych sesjach, repozytoriach i czasie.

Najważniejsze metryki:

- accepted steps per input token,
- czas do pierwszego zaakceptowanego patcha,
- liczba retry na zaakceptowany krok,
- context precision,
- context recall,
- NDCG lub MRR rankingu,
- udział zadań zakończonych w zadanym budżecie.

Przed włączeniem modelu należy zastosować shadow mode: model zapisuje własny
ranking, ale produkcyjnie działa heurystyka. Pozwala to porównać decyzje bez
wpływania na użytkownika.

## Integracja techniczna

Preferowana architektura:

- trening offline w Pythonie,
- eksport prostego modelu jako JSON ze współczynnikami albo ONNX,
- inferencja w procesie Rust,
- brak uruchamiania nowego procesu Python przy każdym kroku,
- jawna wersja schematu cech i modelu,
- automatyczny fallback do heurystyki.

Alternatywnie podczas eksperymentów może działać długowieczny lokalny proces
`pair-ranker` komunikujący się po stdio.

## Warunek rozpoczęcia prac

Implementację ML warto rozpocząć dopiero wtedy, gdy:

- deterministyczny generator kandydatów i ranking mają stabilny kontrakt,
- telemetria obejmuje wynik interakcji użytkownika,
- dostępna jest wystarczająca liczba niezależnych sesji,
- istnieje offline replay i baseline heurystyczny,
- można wykazać poprawę kosztu lub skuteczności w shadow mode.

Do tego czasu zbieramy wersjonowane cechy i decyzje, ale bieżący produkt działa
wyłącznie na regułach deterministycznych.
