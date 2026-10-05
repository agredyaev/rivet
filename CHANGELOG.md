# Changelog

## [0.2.12](https://github.com/agredyaev/rivet/compare/v0.2.11...v0.2.12) (2026-10-05)


### Bug Fixes

* make command execution mode explicit ([c3038f3](https://github.com/agredyaev/rivet/commit/c3038f335c0d7081ea7afb413e5ec29c0c653000))
* make command execution mode explicit ([eb0282e](https://github.com/agredyaev/rivet/commit/eb0282e384456ac0fa10477aa8d4e173e4133d2b))

## [0.2.11](https://github.com/agredyaev/rivet/compare/v0.2.10...v0.2.11) (2026-10-04)


### Bug Fixes

* **ci:** gate unix-only Duration import ([1bfc5bc](https://github.com/agredyaev/rivet/commit/1bfc5bc0431e592d60be2ee1779209c0b8a98211))
* **ci:** restore valid workflow execution ([06bddd6](https://github.com/agredyaev/rivet/commit/06bddd63719bbde3934aa050674e985cfa4615f5))
* detach long run_command executions ([b0f5224](https://github.com/agredyaev/rivet/commit/b0f522443b83d3fd3e62845ab37e823946343b46))
* detach long run_command executions ([5e98690](https://github.com/agredyaev/rivet/commit/5e986904c95e6903fc5bf98a7d5e1e086b195b77))
* detach long run_command executions ([f3b46be](https://github.com/agredyaev/rivet/commit/f3b46be21ab8dbea44e0f76a7389246eb4e4f4ae))
* detach long run_command executions ([d7949a4](https://github.com/agredyaev/rivet/commit/d7949a4d95849a15350ab98a7fd6065f261eac8e))
* detach long run_command executions ([7623c0c](https://github.com/agredyaev/rivet/commit/7623c0cebdebc4e28d318306f477b71ab125a84f))
* preserve background capacity with foreground burst ([0da5cb5](https://github.com/agredyaev/rivet/commit/0da5cb528249ff33e883e1fc8707efbe9cdd63fc))
* report gaps when ready retention is empty ([6b8935f](https://github.com/agredyaev/rivet/commit/6b8935fbc12cad5e9356ba4f44f5c1663085ab69))
* separate process capacity from retained state ([2bb4c90](https://github.com/agredyaev/rivet/commit/2bb4c9053ed17236d131dbb3af3f4ec972ca18a6))
* separate process capacity from retained state ([40493ed](https://github.com/agredyaev/rivet/commit/40493eddd59dbca3c9a62c13054ac4b1cdef52cc))
* separate process capacity from retained state ([5503114](https://github.com/agredyaev/rivet/commit/5503114076fd8712e0421393c84a2f033bc3fa6e))
* separate process capacity from retained state ([31668f7](https://github.com/agredyaev/rivet/commit/31668f76c7c6a236bd5ca7a0d5e7913be15041a3))
* separate process capacity from retained state ([463c8f3](https://github.com/agredyaev/rivet/commit/463c8f3b511a27802ab6fe31de6ec46730d9f153))
* separate process capacity from retained state ([1767c5c](https://github.com/agredyaev/rivet/commit/1767c5ccbed27ebc6188c3a5146db429703e5388))
* separate process capacity from retained state ([7fc6eb6](https://github.com/agredyaev/rivet/commit/7fc6eb64c22d46922a1f7a2805568ad9b0d71b34))
* separate process capacity from retained state ([9400071](https://github.com/agredyaev/rivet/commit/940007156f296aaa5f533850ec50270b4a436a4c))


### Performance Improvements

* return bounded ready output in one MCP response ([ed7750c](https://github.com/agredyaev/rivet/commit/ed7750c5296a98a6ca269d5d88e6d99778add9db))


### Refactoring

* queue process completions without foreground waiting ([6fc1632](https://github.com/agredyaev/rivet/commit/6fc16328c34bf65bf7066a659733373e64bd4d9e))

## [0.2.10](https://github.com/agredyaev/rivet/compare/v0.2.9...v0.2.10) (2026-10-03)


### Bug Fixes

* preserve PowerShell regex during installer update ([30d503d](https://github.com/agredyaev/rivet/commit/30d503d210ec7a7a9bf3dec66ae5d6fda7067d60))
* refuse stale GitHub releases during install ([3bb52dd](https://github.com/agredyaev/rivet/commit/3bb52dd8e8859c66f1ff0ea8e3ffd45d23828e43))
* refuse stale GitHub releases during install ([ffa0d3a](https://github.com/agredyaev/rivet/commit/ffa0d3ab843a7369a70ce31f9ed9b57c28e0bc4c))
* repair PowerShell installer release selection ([bbe0bb3](https://github.com/agredyaev/rivet/commit/bbe0bb3b11f10caf7fe26e33a63c399cb0f0d3e3))

## [0.2.9](https://github.com/agredyaev/rivet/compare/v0.2.8...v0.2.9) (2026-10-03)


### Bug Fixes

* bound tunnel setup steps and expose progress ([a2ac674](https://github.com/agredyaev/rivet/commit/a2ac674a076c39864e1e1d0c64819c281ba9f212))

## [0.2.8](https://github.com/agredyaev/rivet/compare/v0.2.7...v0.2.8) (2026-10-03)


### Bug Fixes

* show tunnel id input during session setup ([667fbe4](https://github.com/agredyaev/rivet/commit/667fbe4600111be87667b39621a3dec8433b533d))

## [0.2.7](https://github.com/agredyaev/rivet/compare/v0.2.6...v0.2.7) (2026-10-03)


### Refactoring

* simplify session setup and release publishing ([ffcf66c](https://github.com/agredyaev/rivet/commit/ffcf66c9000e3a0763e55b48bce2bc086b404fd8))

## [0.2.6](https://github.com/agredyaev/rivet/compare/v0.2.5...v0.2.6) (2026-10-03)


### Bug Fixes

* **cli:** add config-independent help and validate installers ([b731c31](https://github.com/agredyaev/rivet/commit/b731c31b3a115b62169d963ad0d1679f1fd6a047))
* **commands:** preserve cargo shim name ([5624a46](https://github.com/agredyaev/rivet/commit/5624a46cba6647ce8b9d32f46fca56a3e1934702))
* **test:** use portable help test path ([f461892](https://github.com/agredyaev/rivet/commit/f461892a66d898be65e2a5e7c6fa5ef3e0722057))
