# Angular 22 reactivity and Signal Forms benchmark

Status: planned benchmark specification

## Question

Can ProjectProfile and a version-matched Angular Skill make weaker models
perform a semantically correct Angular 22 reactivity migration, rather than a
surface-level replacement of `@Input()` with code that merely looks signal
based?

The benchmark must distinguish four abilities:

1. migrating decorator inputs to the correct `input()` contract;
2. preserving every TypeScript and template read after inputs become callable;
3. moving writable local state to `signal()` and synchronous derivations to
   `computed()` without copying state through `effect()`;
4. using the stable Angular 22 Signal Forms API instead of falling back to
   legacy Reactive Forms or hallucinated intermediate APIs.

Primary references:

- [Signal input migration](https://angular.dev/reference/migrations/signal-inputs)
- [Component inputs](https://angular.dev/guide/components/inputs)
- [Angular Signals](https://angular.dev/guide/signals)
- [Signal Forms essentials](https://angular.dev/essentials/signal-forms)
- [`form()` API](https://angular.dev/api/forms/signals/form)
- [Signal Form validation](https://angular.dev/guide/forms/signals/validation)

## Controlled variants

Every case uses the same current engine, backend, prompt, active buffer, and
fixture. Only the project-intelligence context changes:

- `before`: no ProjectProfile and no selected Skills;
- `profile`: ProjectProfile identifies Angular 22, TypeScript 6, the workspace
  area, and available commands, but supplies no Angular API instructions;
- `after`: the same profile plus a selected, versioned Angular 22 Signals and
  Signal Forms Skill.

The Skill may explain public API contracts and common semantic traps, but must
not contain fixture names, fixture-specific code, rubric phrases, or a complete
answer. Its exact bytes and content hash must be recorded with the run.

## Case 1: signal input contract

### Starting behavior

The component has both a required input and an input with a default value:

```ts
@Input({required: true}) products!: readonly Product[];
@Input() currency = 'PLN';
```

The values are read from TypeScript, the inline template, and a host binding.
The prompt asks for a production-safe migration to signal inputs without
changing public input names or behavior.

### Required result

The implementation must preserve the two different contracts:

```ts
readonly products = input.required<readonly Product[]>();
readonly currency = input('PLN');
```

All reads must call the signal, including reads outside the class body that are
reachable through the fixture. The model must not write to either InputSignal.

### Deterministic checks

- Angular compilation succeeds with strict template checking.
- No `@Input`, `Input` import, or decorator-only compatibility wrapper remains.
- The required input uses `input.required()`; it is not weakened to an optional
  input or given an invented default.
- The defaulted input remains optional with the original default.
- TypeScript, template, and host-binding reads use signal getters.
- Updating inputs through `componentRef.setInput()` updates rendered output.
- No `.set()` or `.update()` is called on an InputSignal.

## Case 2: derived component state

### Starting behavior

The component imperatively maintains duplicated state:

```ts
@Input({required: true}) products!: readonly Product[];
query = '';
filteredProducts: readonly Product[] = [];
total = 0;

ngOnChanges(): void {
  this.refresh();
}

setQuery(query: string): void {
  this.query = query;
  this.refresh();
}
```

`refresh()` filters products and calculates the visible total. The prompt asks
for a signal-native implementation with no manual synchronization method.

### Required result

Ownership must be explicit:

- parent-owned data becomes a read-only required signal input;
- locally edited query becomes a writable `signal()`;
- filtered products and total become read-only `computed()` derivations;
- template reads call each signal;
- synchronous derived state is not propagated with `effect()`.

A representative shape is:

```ts
readonly products = input.required<readonly Product[]>();
readonly query = signal('');
readonly filteredProducts = computed(() => /* derive from products and query */);
readonly total = computed(() => /* derive from filteredProducts */);
```

The representative shape documents ownership, not the fixture's exact answer.

### Deterministic checks

- Angular and TypeScript compilation succeeds.
- Changing the input invalidates both computed values.
- `query.set()` invalidates filtering and total without calling a refresh
  method.
- Repeated reads with unchanged dependencies preserve computed semantics.
- `ngOnChanges`, duplicated writable derived fields, and `refresh()` are gone.
- No `effect()` exists whose only purpose is copying one signal into another.
- Filtering and total calculations preserve the original edge cases, ordering,
  and currency behavior.

## Case 3: Angular 22 Signal Forms

### Starting behavior

A checkout editor uses legacy Reactive Forms and separately recalculates its
total. It contains email, quantity, and unit-price fields with required, email,
and minimum-value validation.

The prompt asks for migration to Angular 22 Signal Forms while preserving
validation, DOM behavior, and total calculation.

### Required result

The form model must have one writable signal owner, and `form()` must create the
field tree:

```ts
readonly checkoutModel = signal({
  email: '',
  quantity: 1,
  unitPrice: 0,
});

readonly checkoutForm = form(this.checkoutModel, path => {
  required(path.email);
  email(path.email);
  min(path.quantity, 1);
});

readonly total = computed(() => {
  const value = this.checkoutModel();
  return value.quantity * value.unitPrice;
});
```

Controls must bind through `[formField]`, and field state must be read through
the callable FieldTree node, for example
`checkoutForm.email().invalid()`.

### Deterministic checks

- The fixture compiles against the pinned Angular 22 and TypeScript 6 lockfile.
- Signal Forms symbols come from `@angular/forms/signals`.
- The component imports `FormField` where the standalone template requires it.
- Inputs bind with `[formField]`; legacy `[formControl]`, `formControlName`, and
  `FormGroup` are absent.
- DOM input updates synchronize the writable model signal.
- Required, email, and minimum validation react to model and DOM changes.
- `invalid()`, `errors()`, `dirty()`, and `touched()` are called at the correct
  FieldTree level where the fixture uses them.
- Total is a `computed()` derivation of the model, not an independently writable
  field or submit-time calculation.
- The implementation preserves disabled, submit, and error-display behavior
  explicitly present in the fixture.

## Case 4: integrated catalog filter form

This is the hard case and should be reported separately from the three focused
cases. A required product input feeds a local Signal Form containing query and
price-range controls. Computed results derive from both ownership domains:

```text
parent product input ─┐
                     ├─> computed visible products ─> computed total
local form model ─────┘
```

The solution must not copy the input into writable local state merely to make
it editable. The form owns only user-entered filters; the parent continues to
own the products. Runtime tests update parent inputs and form controls in both
orders to detect stale snapshots and one-way synchronization.

## Scoring

`Pass` requires all of the following:

1. the required Patch card kind;
2. a patch that applies through Loopbiotic's shared typed-hunk path;
3. Angular/TypeScript compilation;
4. all runtime and template tests;
5. every case-specific semantic rubric item.

Report separate component scores so failures remain diagnosable:

| Dimension | Meaning |
| --- | --- |
| Contract | Correct Patch card, target file, and applicable structured hunk |
| Compile | Angular compiler and strict template checks pass |
| Input migration | Required/default semantics and every callable read are correct |
| Reactivity | Writable ownership and computed invalidation are correct |
| Signal Forms | Current imports, FieldTree access, bindings, and validation are correct |
| Behavior | Runtime tests preserve observable behavior |
| Anti-patterns | No copied derivations, illegal InputSignal writes, or legacy fallback |

Also record pass rate, rubric content, accepted card kind, input/cached/output
tokens, wall time, time to first visible progress, attempts, violation classes,
and bounded tool calls. Tokens remain comparable only between variants of one
model/provider path.

## Model matrix and sample

Initial matrix:

- Qwen3.5 9B;
- Gemma 4 12B;
- Qwen3.6 35B-A3B;
- GPT-5.6 Sol at low effort.

Run three repetitions of each focused case and variant: 9 responses per
model/variant, 27 per model, and 108 focused-case responses total. Run the
integrated hard case as a separately labeled extension so it cannot hide which
primitive capability failed. Use temperature 0 and a fixed seed where the
provider supports them; record every unsupported determinism control.

## Fixture and execution rules

- Pin exact Angular 22, TypeScript 6, and test-runner versions in a lockfile.
- Install dependencies before measurement; no benchmark turn may use the
  network.
- Copy the fixture into a fresh temporary workspace for every response.
- Keep prompts identical across variants and models, apart from provider-required
  transport wrappers.
- Do not expose rubric phrases, expected diffs, or test source to the model.
- Allow only the source context, deterministic ProjectProfile, selected Skill,
  and explicitly configured bounded read-only project tools.
- Apply the proposed patch only inside the temporary copy, then run the same
  compile and runtime commands.
- Preserve raw cards, normalized patches, diagnostics, tool telemetry, and
  machine-readable JSONL results for failure analysis.

## Failure taxonomy

Classify at least these failures independently:

- `legacy_input`: decorator or stale `Input` import remains;
- `input_contract`: required/default semantics changed;
- `uncalled_signal`: stale property reads remain in TypeScript or template;
- `input_mutation`: code writes to an InputSignal;
- `copied_derivation`: writable state or `effect()` duplicates a derivation;
- `stale_snapshot`: input or form update does not invalidate downstream state;
- `legacy_forms`: solution falls back to Reactive Forms;
- `signal_forms_api`: wrong package, directive, FieldTree, or validator API;
- `template_type_error`: TypeScript passes but strict template compilation fails;
- `behavior_regression`: compile succeeds but runtime semantics differ;
- `output_contract`: card or typed patch output is invalid.

## Acceptance condition

The architecture is successful when the `after` variant materially improves
weak-model compile and runtime pass rates, not only lexical rubric coverage,
while GPT-5.6 reaches the same correct result with lower or unchanged median
latency. Signal Forms success must come from the selected versioned Skill and
detected Angular version, not fixture-specific answer leakage.

This is a framework-migration benchmark, not a general Angular leaderboard.
Results must state the exact API snapshot, fixture commit, backend capabilities,
reasoning setting, tool budget, and hardware profile.
