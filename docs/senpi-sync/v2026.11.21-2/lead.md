# senpi 59d58cee5..v2026.11.21-2: lead rows (repo tooling and non-functional commits)

Rows the per-area tracks do not own. Every functional row of this range is judged in the track ledgers beside this file.

| senpi PR / commit | verdict | reason |
|---|---|---|
| `bc3336c7de` | n/a | sync merge of origin/main; its content is covered by the PR rows of the owning tracks |
| `54a7c2adca` | n/a | sync merge of origin/main; its content is covered by the PR rows of the owning tracks |
| `c130b2d8b9` | n/a | sync merge of origin/main; its content is covered by the PR rows of the owning tracks |
| `0acf766f44` | n/a | sync merge of origin/main; its content is covered by the PR rows of the owning tracks |
| #4312 (`19a5ba8298`) | n/a | Windows native adapter crate `senpi-socket-secret` (presence guards / binding, owner-only DACL, `CREATE_NEW` staging) and its loader `socket-secret-native.ts`: at f01af13491 no senpi production code imports the loader, so senpi still writes the secret in place on win32 like `crates/maho-socket-secret` (same decision as round 1, `crates/maho-socket-secret/parity.d/5179.md`); nothing to match until upstream wires it |
| #4370 (`3d0df57eb3`) | n/a | Windows native adapter crate `senpi-socket-secret` (presence guards / binding, owner-only DACL, `CREATE_NEW` staging) and its loader `socket-secret-native.ts`: at f01af13491 no senpi production code imports the loader, so senpi still writes the secret in place on win32 like `crates/maho-socket-secret` (same decision as round 1, `crates/maho-socket-secret/parity.d/5179.md`); nothing to match until upstream wires it |
| #4373 (`3505ce7c8a`) | n/a | Windows native adapter crate `senpi-socket-secret` (presence guards / binding, owner-only DACL, `CREATE_NEW` staging) and its loader `socket-secret-native.ts`: at f01af13491 no senpi production code imports the loader, so senpi still writes the secret in place on win32 like `crates/maho-socket-secret` (same decision as round 1, `crates/maho-socket-secret/parity.d/5179.md`); nothing to match until upstream wires it |
| #4470 (`3e39075fe0`) | n/a | Windows native adapter crate `senpi-socket-secret` (presence guards / binding, owner-only DACL, `CREATE_NEW` staging) and its loader `socket-secret-native.ts`: at f01af13491 no senpi production code imports the loader, so senpi still writes the secret in place on win32 like `crates/maho-socket-secret` (same decision as round 1, `crates/maho-socket-secret/parity.d/5179.md`); nothing to match until upstream wires it |
| #4547 (`2ff07b0954`) | n/a | knip cleanup: drops the export keyword of bashPermissionRequests; no runtime behavior change |
| #4801 (`cde540c537`) | n/a | senpi CI/release workflows; JS dependency manifests/locks; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #4860 (`ccf76963ab`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #4899 (`f8ff4fdb78`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #4923 (`315a9b7918`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #5411 (`bf03177f93`) | moved | judged in the tools ledger |
| #5718 (`75585eacc3`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #5786 (`b84754b934`) | n/a | TS lint/test config; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #5910 (`deb598af8b`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #5968 (`f35d38e2fa`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6023 (`38b075fd9a`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6140 (`8e4a63f6d9`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6422 (`c5c77595aa`) | moved | judged in the ai ledger |
| #6497 (`5a929a82fa`) | n/a | JS dependency manifests/locks; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6623 (`7031a7bba4`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6625 (`f3114017cc`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6628 (`c665ff4bbd`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6632 (`6f1f57721f`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6638 (`814deca445`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6645 (`c368a69d52`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6649 (`015ef759ed`) | n/a | docs/release notes; JS dependency manifests/locks; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6671 (`af1b559996`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6691 (`8795b1a77c`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6695 (`3ac6f72649`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6697 (`4751bd10ca`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6707 (`1252dbd6c6`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6836 (`bad51c1af5`) | n/a | senpi CI/release workflows; TS lint/test config; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6847 (`6c92bc9a73`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6848 (`ac57963ceb`) | n/a | senpi agent QA skill assets; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6855 (`feb1a9f204`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6861 (`f8fe8dab2c`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6867 (`aa517922f1`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6870 (`d465582274`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #6944 (`414c68acef`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #7000 (`eade1fa1db`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #7011 (`9bc8b85ac5`) | n/a | senpi CI/release workflows; senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #7013 (`73dcd290e4`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| #7031 (`aecb53fef5`) | n/a | senpi repo scripts (release, CI sharding, lint ratchet, test guards) only; no Rust counterpart |
| `c53eb0ead2` | n/a | changelog bookkeeping |
| `a23c250c2b` | n/a | changelog bookkeeping |
| `592b133504` | n/a | changelog bookkeeping |
| `99203d2d82` | n/a | changelog bookkeeping |
| `537e2ba87c` | n/a | changelog bookkeeping |
| `c0a32ad544` | n/a | changelog bookkeeping |
| `e0c53247ed` | n/a | changelog bookkeeping |
| `ed4e8d6021` | n/a | changelog bookkeeping |
| `5634be278a` | n/a | changelog bookkeeping |
| `292fd11c04` | n/a | changelog bookkeeping |
| `9d1c49f0f9` | n/a | changelog bookkeeping |
| `25fbac7216` | n/a | changelog bookkeeping |
| `ce0b3c711d` | n/a | changelog bookkeeping |
| `5456d60de5` | n/a | changelog bookkeeping |
| `927f7838c1` | n/a | changelog bookkeeping |
| `ac26a1381b` | n/a | changelog bookkeeping |
| `71ca920f12` | n/a | changelog bookkeeping |
| `9084994067` | n/a | changelog bookkeeping |
| `0e540dbf28` | n/a | changelog bookkeeping |
| `2e8907611f` | n/a | changelog bookkeeping |
| `eb48e1a3cd` | n/a | changelog bookkeeping |
| `b1515fea7f` | n/a | changelog bookkeeping |
| `7f3f268026` | n/a | changelog bookkeeping |
| `e7b09aeabd` | n/a | changelog bookkeeping |
| `632a44b2aa` | n/a | changelog bookkeeping |
| `b9a295a47e` | n/a | changelog bookkeeping |
| `0547cf0674` | n/a | changelog bookkeeping |
| `69b864aa70` | n/a | changelog bookkeeping |
| `eb5968f096` | n/a | changelog bookkeeping |
| `efc3e4b2ce` | n/a | changelog bookkeeping |
| `186eda3f35` | n/a | changelog bookkeeping |
| `ceedde21f8` | n/a | changelog bookkeeping |
| `29b6da58fb` | n/a | changelog bookkeeping |
| `be1fc3478b` | n/a | changelog bookkeeping |
| `4f0c1a64b1` | n/a | changelog bookkeeping |
| `e07ee94f1e` | n/a | changelog bookkeeping |
| `87f05caa96` | n/a | changelog bookkeeping |
| `23d5d8846f` | n/a | changelog bookkeeping |
| `98aac407a1` | n/a | changelog bookkeeping |
| `57a1c051ca` | n/a | changelog bookkeeping |
| `808d463fbb` | n/a | changelog bookkeeping |
| `a55d00c2db` | n/a | changelog bookkeeping |
| #4483 (`39dcd1b7f6`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4611 (`91f3068fec`) | n/a | senpi CI/release workflows only; no Rust counterpart |
| #4612 (`3cd7cc5678`) | n/a | senpi CI/release workflows only; no Rust counterpart |
| #4661 (`03a352b7bb`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4664 (`e720b07247`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4665 (`23932ee19c`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4716 (`b2bdaeb030`) | n/a | senpi CI/release workflows only; no Rust counterpart |
| #4751 (`c30dc42821`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4770 (`3fb806e73c`) | n/a | senpi CI/release workflows; JS dependency manifests/locks only; no Rust counterpart |
| #4872 (`709b92522c`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4876 (`6bc131a404`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4882 (`ae5826b96a`) | n/a | TS lint/test config only; no Rust counterpart |
| #4915 (`a852773380`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4983 (`b98efd2e86`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #4989 (`327bafcf0c`) | n/a | senpi agent QA skill assets; senpi CI/release workflows only; no Rust counterpart |
| #5083 (`656a943f50`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5094 (`fa8861d12b`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5124 (`71ab0ef559`) | n/a | TS lint/test config only; no Rust counterpart |
| #5129 (`670c8ea5eb`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5172 (`b338523410`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5182 (`c17921b923`) | n/a | senpi agent QA skill assets; senpi CI/release workflows only; no Rust counterpart |
| #5213 (`c73658ee72`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5238 (`e5bc30f1d9`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5261 (`827d26cbaf`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5271 (`a3d668cab3`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5295 (`3ab9fec999`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5566 (`a1b5183a51`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5633 (`ecd958ee53`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #5765 (`0c2299cfdb`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6066 (`24ac212547`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6224 (`89d0ed2e29`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6355 (`4e484b67be`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6624 (`cafea85573`) | n/a | JS dependency manifests/locks only; no Rust counterpart |
| #6722 (`5bbdd9df93`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6842 (`fa0f92b0a0`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6845 (`2edb4c51dc`) | n/a | senpi agent QA skill assets only; no Rust counterpart |
| #6856 (`f97ea7dedb`) | n/a | JS dependency manifests/locks only; no Rust counterpart |
| #6963 (`1a3c67ed3d`) | n/a | TS lint/test config only; no Rust counterpart |
| #6967 (`cacacc27d4`) | n/a |  only; no Rust counterpart |
| #7090 (`6d4426a28c`) | n/a | senpi CI/release workflows only; no Rust counterpart |
