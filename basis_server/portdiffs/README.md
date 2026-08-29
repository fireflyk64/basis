# Port diffs, module by module

One file per module, `<module>portdiffs.md`, comparing the C# original in `Basis Server/` with
the Rust port in `basis_server/`. Each records three things and distinguishes them carefully:

* **Deviations** — where the Rust behaves differently from the C#, deliberately or not. Every one
  should say *why*, and whether a test pins it.
* **Corners cut** — anything the port simplified, omitted, stubbed or left less capable than the
  original. This is the section to read before trusting the port with something new.
* **Improvements** — where the Rust is stronger than the C#: a bound the original lacks, an error
  the original swallowed, a race the original had.

"No deviation" is a real and common answer; a file that reads the same in both languages is
recorded as such rather than padded.
