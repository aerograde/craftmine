# Visual Vibrance pack

`Visual-Vibrance-v0.3.5a.zip` is the original shader pack by **Josh Britain
(jbritain)**, © 2025. It is redistributed **unmodified** under its own
source-available, non-commercial license (not OSI open source). The `LICENSE`
inside the zip is part of the distribution and must stay there. Credit:
<https://jbritain.net>.

The expected SHA-256 of the unmodified zip is:

```
0bf94c357b7d3a7312aa828edf892354c25273931543d0087b5577aad4fb76f0
```

`tests/test_visual_vibrance_pack.py` checks both this file and the copy embedded
in `web/out/horizon.html` (when present) against that hash. The built-in fast
post profile is a separate Horizon implementation; it is not claimed to be a
byte rewrite of this pack. Selecting the pack runs the original code through
Horizon's Iris-format translator.
