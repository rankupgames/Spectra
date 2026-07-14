# Code adapters

Spectra's parsing lives behind a language-adapter registry. The registry is the single source of truth for file discovery, grammar dispatch, and cache identity; an unchanged fragment is reused only when both its content hash and detected adapter match.

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

C, C++, Dart, Lua, and Luau already have registered parser-backed core and semantic adapters. Their `Planned` cells below refer only to additional ecosystem-specific routing or cross-language bridges; v0.3 does not reimplement their extraction.

## Current adapter pack

| Language | Extensions | Core extraction | Semantic edges | Routing / bridges |
| --- | --- | --- | --- | --- |
| Rust | `.rs` | Implemented | Calls, imports, trait implementations | Rocket, Axum, and Actix route-to-handler bridges implemented |
| TypeScript / TSX | `.ts`, `.tsx` | Implemented | Calls, imports, extends, implements | Express/NestJS and React/Next routes, JSX renders, TurboModule specs, and Fabric components implemented |
| JavaScript / JSX | `.js`, `.jsx`, `.mjs`, `.cjs` | Implemented | Calls, imports, extends | Express/NestJS and React/Next routes, JSX renders, and React Native calls implemented |
| Python | `.py` | Implemented | Calls, imports, inheritance | Django, DRF, Flask, and FastAPI routes implemented |
| Go | `.go` | Implemented | Calls, package imports | Gin/Echo/Mux/Chi-style routes and GoFrame route metadata implemented |
| Java | `.java` | Implemented | Calls, imports, extends, implements | Spring routes, Play handlers, React Native methods, and Fabric view managers implemented |
| C | `.c`, `.h` | Implemented | Calls, includes | Planned |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | Implemented | Calls, includes, inheritance | Planned |
| C# | `.cs` | Implemented | Calls, imports, extends, implements | ASP.NET controller and minimal-API routes implemented |
| PHP | `.php` | Implemented | Calls, imports/includes, extends, implements | Laravel explicit and resource routes implemented |
| Ruby | `.rb` | Implemented | Calls, requires, inheritance | Rails explicit and resource routes implemented |
| Swift | `.swift` | Implemented | Calls, imports, inheritance and protocol adoption | Vapor routes, SwiftUI components, and Expo Module exports implemented |
| Kotlin | `.kt`, `.kts` | Implemented | Calls, imports, inheritance and interface implementation | Spring routes plus Expo Module and Fabric/React Native exports implemented |
| Scala | `.scala`, `.sc`, Play `conf/routes` | Implemented | Calls, imports, inheritance and trait mixins | Play route-to-handler bridges implemented |
| Dart | `.dart` | Implemented | Calls, imports, extends, implements, and mixins | Planned |
| Lua | `.lua` | Implemented | Calls and module requires | Planned |
| Luau | `.luau` | Implemented | Calls, module requires, and type aliases | Planned |
| Svelte | `.svelte` | Implemented, including embedded JS/TS | Calls, imports, component renders, event bindings | SvelteKit `+page` routes and script bridges implemented |
| Vue | `.vue` | Implemented, including embedded JS/TS | Calls, imports, component renders, event bindings | Nuxt page routes and script-setup bridges implemented |
| Astro | `.astro` | Implemented, including frontmatter TS | Calls, imports, component renders, event bindings | Astro page routes and frontmatter bridges implemented |
| Liquid | `.liquid` | Implemented over HTML structure | Render/include and output bindings | Template-to-snippet and output bridges implemented |
| Objective-C | `.m`, `.mm` | Implemented | Calls, imports, inheritance, protocols, implementations, message sends | C/Objective-C resolution plus React Native exports and Fabric view managers implemented |
| CUDA | `.cu`, `.cuh` | Implemented | Calls, includes, inheritance, kernel launches | C++-family definitions and CUDA kernels share the common resolver |
| Metal | `.metal` | Implemented over the C++ grammar | Calls, includes, inheritance, shader entry points | C++-family definitions and Metal kernels share the common resolver |
| R | `.r`, `.R` | Implemented | Functions, S4 classes/generics/methods | Package imports and calls implemented |
| Nix | `.nix` | Implemented | Attribute bindings and functions | `import` and `callPackage` bridges implemented |
| Erlang | `.erl`, `.hrl`, `.escript`, `.app`, `.app.src` | Implemented | Modules, functions, behaviours | Header imports, local and remote calls implemented |
| Solidity | `.sol` | Implemented | Contracts, interfaces, libraries, functions, modifiers, events | Imports, inheritance, and calls implemented |
| Terraform / OpenTofu | `.tf`, `.tfvars`, `.tofu` | Implemented | Resource, data, module, variable, output, and provider blocks | Module sources and expression references implemented |
| Pascal / Delphi | `.pas`, `.dpr`, `.dpk`, `.lpr`, `.dfm`, `.fmx` | Implemented | Units, programs, classes, procedures, functions | Uses/imports, inheritance, and calls implemented |
| ArkTS | `.ets` | Implemented over the TypeScript grammar plus ArkUI extraction | TypeScript symbols and ArkUI components | ArkUI navigation routes implemented |
| Razor | `.cshtml`, `.razor` | Implemented | Components and code-block methods | Blazor routes, component renders, and event bindings implemented |
| Visual Basic .NET | `.vb` | Implemented | Namespaces, modules, types, methods | Imports, inheritance, implementations, and calls implemented |
| CFML / CFScript / CFQuery | `.cfc`, `.cfm`, `.cfs` | Implemented | Components, functions, and queries | Includes, template imports, and calls implemented |
| COBOL | `.cbl`, `.cob`, `.cobol`, `.cpy` | Implemented | Programs, sections, and paragraphs | COPY, CALL, PERFORM, and CICS LINK edges implemented |
| YAML | `.yml`, `.yaml` | Implemented | Nested configuration leaf symbols | Drupal controller routes and placeholder references implemented |
| Twig | `.twig` | Implemented | Templates, blocks, and macros | Extends/includes/imports and output bindings implemented |
| XML | `.xml` | Implemented for structured mapper content | MyBatis namespaces, statements, and fragments | Statement-to-mapper method bindings and result-map references implemented |
| Properties | `.properties` | Implemented | Configuration key symbols | Placeholder references implemented |

The common resolver prefers exact-case definitions compatible with the edge type, then falls back to case-insensitive matching. Multiple eligible candidates produce a typed uncertain boundary, preserving ambiguity in the rendered topology. Conventional page routes use local `routes_to` edges, while embedded script symbols remain contained by their component and participate in normal cross-file resolution.

## Parity baseline

The baseline is CodeGraph v1.3.0 as installed when v0.2 development began. Its complete language and extension surface is represented by the 39 adapters above. CUDA and Metal are C++ dialects in CodeGraph; Spectra uses dedicated adapters for their kernel semantics while preserving C++-family resolution. Erlang application manifests (`.app` and `.app.src`) are recognized through path-aware detection.

Parity is pinned to this baseline so a moving upstream language list cannot silently change the release gate. The registry, extraction fixtures, route-resolution tests, and native-bridge tests enforce that surface. The reviewed v0.2 real-repository gate found all 22 CodeGraph route labels in Spectra and 40 Spectra routes overall across the pinned FastAPI/React, Laravel, NestJS, Spring MVC, and Vapor corpus. The topology corpus and provider-backed multimodal results are recorded in the [v0.2 baseline](../benchmarks/v0.2-baseline.md). Later CodeGraph additions can be adopted deliberately without silently changing the v0.2 contract.
