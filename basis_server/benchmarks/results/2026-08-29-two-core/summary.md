
### 25 players
| metric | csharp-legacy |
|---|---|
| delivered Hz/pair | 83.33 |
| delivery ratio | 1 |
| server cores | 0.081 |
| crowd cores | 0.029 |
| egress MB/s | 0.173 |
| datagrams/s | 1385 |
| avatar drops/s | 0 |
| voice drops/s | 0 |
| slice count | 1 |
| tick ms | 0.222 |
| overrun ratio | 0 |
| committed MB | 13.8 |
| voice heard | 1 |
  csharp-legacy: 25 connected, 4 windows

### 50 players
| metric | csharp-legacy | rust-legacy | rust-mix0 | rust-mix0.5 | rust-legacy / csharp-legacy | rust-mix0 / csharp-legacy | rust-mix0.5 / csharp-legacy |
|---|---|---|---|---|---|---|---|
| delivered Hz/pair | 83.33 | 83.33 | 83.33 | 83.33 | 1.000x | 1.000x | 1.000x |
| delivery ratio | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| server cores | 0.125 | 0.198 | 0.271 | 0.287 | 1.584x | 2.168x | 2.296x |
| crowd cores | 0.061 | 0.064 | 0.342 | 0.233 | 1.049x | 5.607x | 3.820x |
| egress MB/s | 0.684 | 0.781 | 0.745 | 0.708 | 1.142x | 1.089x | 1.035x |
| datagrams/s | 3924 | 4293 | 1.113e+04 | 7382 | 1.094x | 2.836x | 1.881x |
| avatar drops/s | 0 | 0 | 0 | 0 | - | - | - |
| voice drops/s | 0 | 0 | 0 | 0 | - | - | - |
| slice count | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| tick ms | 0.478 | 0.483 | 1.745 | 1.264 | 1.010x | 3.651x | 2.644x |
| overrun ratio | 0 | 0 | 0 | 0 | - | - | - |
| committed MB | 19.1 | 0 | 0 | 0 | 0.000x | 0.000x | 0.000x |
| voice heard | 0.9942 | 0.9944 | 0.992 | 0.9907 | 1.000x | 0.998x | 0.996x |
  csharp-legacy: 50 connected, 4 windows
  rust-legacy: 50 connected, 4 windows
  rust-mix0: 50 connected, 4 windows
  rust-mix0.5: 50 connected, 4 windows

### 100 players
| metric | csharp-legacy | rust-legacy | rust-mix0 | rust-mix0.5 | rust-legacy / csharp-legacy | rust-mix0 / csharp-legacy | rust-mix0.5 / csharp-legacy |
|---|---|---|---|---|---|---|---|
| delivered Hz/pair | 83.33 | 83.33 | 83.33 | 83.33 | 1.000x | 1.000x | 1.000x |
| delivery ratio | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| server cores | 0.231 | 0.351 | 0.349 | 0.27 | 1.519x | 1.511x | 1.169x |
| crowd cores | 0.137 | 0.139 | 0.523 | 0.354 | 1.015x | 3.818x | 2.584x |
| egress MB/s | 2.981 | 3.059 | 3.295 | 2.828 | 1.026x | 1.105x | 0.949x |
| datagrams/s | 9728 | 9735 | 4.901e+04 | 2.552e+04 | 1.001x | 5.038x | 2.623x |
| avatar drops/s | 0 | 0 | 0 | 0 | - | - | - |
| voice drops/s | 0 | 0 | 0 | 0 | - | - | - |
| slice count | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| tick ms | 1.181 | 1.214 | 2.16 | 1.345 | 1.028x | 1.829x | 1.139x |
| overrun ratio | 0 | 0 | 0.0039 | 0 | - | - | - |
| committed MB | 25.3 | 0 | 0 | 0 | 0.000x | 0.000x | 0.000x |
| voice heard | 0.9939 | 0.9939 | 0.9974 | 0.9934 | 1.000x | 1.004x | 0.999x |
  csharp-legacy: 100 connected, 4 windows
  rust-legacy: 100 connected, 4 windows
  rust-mix0: 100 connected, 4 windows
  rust-mix0.5: 100 connected, 4 windows

### 200 players
| metric | csharp-legacy | rust-legacy | rust-mix0 | rust-mix0.5 | rust-legacy / csharp-legacy | rust-mix0 / csharp-legacy | rust-mix0.5 / csharp-legacy |
|---|---|---|---|---|---|---|---|
| delivered Hz/pair | 83.33 | 83.33 | 83.33 | 83.33 | 1.000x | 1.000x | 1.000x |
| delivery ratio | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| server cores | 0.405 | 0.375 | 0.699 | 0.535 | 0.926x | 1.726x | 1.321x |
| crowd cores | 0.359 | 0.333 | 0.798 | 0.497 | 0.928x | 2.223x | 1.384x |
| egress MB/s | 11.7 | 11.62 | 11.32 | 11.72 | 0.993x | 0.968x | 1.001x |
| datagrams/s | 2.332e+04 | 2.308e+04 | 1.662e+05 | 9.713e+04 | 0.990x | 7.126x | 4.166x |
| avatar drops/s | 0 | 0 | 0 | 0 | - | - | - |
| voice drops/s | 0 | 0 | 0 | 0 | - | - | - |
| slice count | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| tick ms | 2.625 | 1.81 | 4.621 | 3.279 | 0.690x | 1.760x | 1.249x |
| overrun ratio | 0.0312 | 0 | 0.0215 | 0 | 0.000x | 0.689x | 0.000x |
| committed MB | 52 | 0 | 0 | 0 | 0.000x | 0.000x | 0.000x |
| voice heard | 0.9941 | 0.9948 | 0.9917 | 0.9944 | 1.001x | 0.998x | 1.000x |
  csharp-legacy: 200 connected, 4 windows
  rust-legacy: 200 connected, 4 windows
  rust-mix0: 200 connected, 4 windows
  rust-mix0.5: 200 connected, 4 windows

### 400 players
| metric | csharp-legacy | rust-legacy | rust-mix0 | rust-mix0.5 | rust-legacy / csharp-legacy | rust-mix0 / csharp-legacy | rust-mix0.5 / csharp-legacy |
|---|---|---|---|---|---|---|---|
| delivered Hz/pair | 46.48 | 50.5 | 37.5 | 9.375 | 1.086x | 0.807x | 0.202x |
| delivery ratio | 1 | 1 | 1 | 1 | 1.000x | 1.000x | 1.000x |
| server cores | 0.663 | 0.672 | 0.873 | 0.784 | 1.014x | 1.317x | 1.183x |
| crowd cores | 0.6 | 0.579 | 0.817 | 0.768 | 0.965x | 1.362x | 1.280x |
| egress MB/s | 41.54 | 42.91 | 26.14 | 30.35 | 1.033x | 0.629x | 0.731x |
| datagrams/s | 5.886e+04 | 5.682e+04 | 3.43e+05 | 2.17e+05 | 0.965x | 5.828x | 3.687x |
| avatar drops/s | 0 | 0 | 0.1 | 0 | - | - | - |
| voice drops/s | 0 | 0 | 0 | 0 | - | - | - |
| slice count | 1.16 | 1.03 | 1.59 | 5.47 | 0.888x | 1.371x | 4.716x |
| tick ms | 12.55 | 11.69 | 14.87 | 14.63 | 0.932x | 1.185x | 1.166x |
| overrun ratio | 0.1777 | 0.125 | 0.1328 | 0.1465 | 0.703x | 0.747x | 0.824x |
| committed MB | 108.7 | 0 | 0 | 0 | 0.000x | 0.000x | 0.000x |
| voice heard | 0.9884 | 0.9923 | 0.4936 | 0.939 | 1.004x | 0.499x | 0.950x |
  csharp-legacy: 400 connected, 4 windows
  rust-legacy: 400 connected, 4 windows
  rust-mix0: 400 connected, 4 windows
  rust-mix0.5: 400 connected, 4 windows
