---
title: Compiler Lab
description: Inspect Nymph tokens, syntax trees, expanded source, diagnostics, and generated JavaScript in your browser.
aside: false
editLink: false
---

# Compiler Lab

Edit a Nymph module and inspect each stage of the real compiler. The **Macro Expansion** panel shows
the formatted runtime module after compile-time expansion reaches its fixed point, with const-only
declarations and macro markers removed. It clears when expansion fails. Everything runs locally in
your browser through WebAssembly; source code is never uploaded.

The **Diagnostics** panel uses the same visual renderer as the command-line compiler, including
source excerpts, carets, secondary labels, notes, help, and macro expansion traces. The browser also
retains the structured diagnostic fields used for source selection and other tooling behavior.

<ClientOnly>
  <NymphDebugger />
</ClientOnly>
