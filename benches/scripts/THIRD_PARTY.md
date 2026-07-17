# Benchmark corpus provenance

The files under `boa/` are copied without modification from
`https://github.com/boa-dev/boa` at commit
`b562725784a18e55d27ddd9702abac61b36a250d`. Boa distributes them under
`MIT OR Unlicense`; `licenses/Boa-LICENSE-MIT` and
`licenses/Boa-LICENSE-UNLICENSE` are the copied license texts.

Every approved script is also listed in `benchmark_config.toml` with its
upstream path and SHA-256. The runner rejects local corpus drift before taking
samples.
