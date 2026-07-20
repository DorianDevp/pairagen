# Angular conventions

This Angular 22 codebase uses signal inputs. A required component input is declared as `readonly title = input.required<string>();`; do not add or retain the legacy `@Input()` decorator. Templates read input signals by calling them, such as `title()`.
