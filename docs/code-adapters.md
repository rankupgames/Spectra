# Code adapters

Spectra v0.2 moves parsing behind a language-adapter registry. The registry is the single source of truth for file discovery, grammar dispatch, and cache identity; an unchanged fragment is reused only when both its content hash and detected adapter match.

## Functional contract

Recognizing an extension is not language support. An adapter is complete only when it provides the relationships that developers use to navigate that ecosystem:

1. Structural symbols and exact source spans.
2. Containment and qualified names.
3. Imports, calls, inheritance, and implementations where applicable.
4. Framework routes when routing conventions define important entry points.
5. Cross-language bridges when normal execution crosses a language boundary.
6. Explicit uncertain boundaries instead of guessed resolved edges.
7. Grammar fixtures plus measured cross-file coverage on representative repositories.

Core extraction, ecosystem routing, and language bridges may land separately, but the parity table calls a language complete only after the applicable layers pass.

## Current adapter pack

| Language | Extensions | Core extraction | Semantic edges | Routing / bridges |
| --- | --- | --- | --- | --- |
| Rust | `.rs` | Implemented | Calls, imports, trait implementations | Planned |
| TypeScript / TSX | `.ts`, `.tsx` | Implemented | Calls, imports, extends, implements | Planned |
| JavaScript / JSX | `.js`, `.jsx`, `.mjs`, `.cjs` | Implemented | Calls, imports, extends | Planned |
| Python | `.py` | Implemented | Calls, imports, inheritance | Planned |
| Go | `.go` | Implemented | Calls, package imports | Planned |
| Java | `.java` | Implemented | Calls, imports, extends, implements | Planned |
| C | `.c`, `.h` | Implemented | Calls, includes | Planned |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | Implemented | Calls, includes, inheritance | Planned |
| C# | `.cs` | Implemented | Calls, imports, extends, implements | Planned |
| PHP | `.php` | Implemented | Calls, imports/includes, extends, implements | Planned |
| Ruby | `.rb` | Implemented | Calls, requires, inheritance | Planned |
| Swift | `.swift` | Implemented | Calls, imports, inheritance and protocol adoption | Planned |
| Kotlin | `.kt`, `.kts` | Implemented | Calls, imports, inheritance and interface implementation | Planned |
| Scala | `.scala`, `.sc` | Implemented | Calls, imports, inheritance and trait mixins | Planned |
| Dart | `.dart` | Implemented | Calls, imports, extends, implements, and mixins | Planned |
| Lua | `.lua` | Implemented | Calls and module requires | Planned |
| Luau | `.luau` | Implemented | Calls, module requires, and type aliases | Planned |
| Svelte | `.svelte` | Implemented, including embedded JS/TS | Calls, imports, component renders, event bindings | SvelteKit `+page` routes and script bridges implemented |
| Vue | `.vue` | Implemented, including embedded JS/TS | Calls, imports, component renders, event bindings | Nuxt page routes and script-setup bridges implemented |
| Astro | `.astro` | Implemented, including frontmatter TS | Calls, imports, component renders, event bindings | Astro page routes and frontmatter bridges implemented |
| Liquid | `.liquid` | Implemented over HTML structure | Render/include and output bindings | Template-to-snippet and output bridges implemented |

The common resolver links a target only when there is one matching definition. Multiple candidates produce a typed uncertain boundary, preserving ambiguity in the rendered topology. Conventional page routes use local `routes_to` edges, while embedded script symbols remain contained by their component and participate in normal cross-file resolution.

## Parity baseline

The baseline is CodeGraph v1.3.0 as installed when v0.2 development began. Remaining families include Objective-C, R, Nix, Erlang, Solidity, Terraform/OpenTofu, Pascal/Delphi, ArkTS, Visual Basic .NET, CFML, COBOL, CUDA, and Metal.

Parity is pinned to this baseline so a moving upstream language list cannot silently change the release gate. Later CodeGraph additions can be adopted deliberately.
