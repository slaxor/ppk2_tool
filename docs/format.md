# `.ppk2` recording format

A recording is one text header line followed by fixed-size binary records.
It is written by `ppk2_tool record`, by `ppk2_tool serve` (as its buffer, and
on `save`), and read by `ppk2_tool-gui <file>`.

## Header

The first line, terminated by `\n`, is ASCII:

    ppk2_tool v1 fold=<N> sps=<S> started_ms=<T>

| key          | meaning                                                      |
|--------------|--------------------------------------------------------------|
| `v1`         | format version; readers must refuse other versions           |
| `fold`       | raw samples summarised per record                            |
| `sps`        | raw sample rate of the device, 100000 for the PPK2           |
| `started_ms` | wall-clock start of the recording, Unix milliseconds (UTC)   |

Unknown `key=value` pairs may appear later; readers ignore them.
Record `i` covers samples `[i·fold, (i+1)·fold)` and therefore starts at
`started_ms + i · fold · 1000 / sps` milliseconds.

## Records

Everything after the header line is a sequence of 16-byte records, little
endian, no padding between records:

| offset | type  | field       | meaning                                          |
|--------|-------|-------------|--------------------------------------------------|
| 0      | f32   | `min`       | lowest current in the window, µA                 |
| 4      | f32   | `max`       | highest current in the window, µA                |
| 8      | f32   | `mean`      | mean current in the window, µA                   |
| 12     | u8    | `logic_any` | bits that were high in at least one sample       |
| 13     | u8    | `logic_all` | bits that were high in every sample              |
| 14     | u16   | `dropped`   | frames lost before this record (counter gaps), saturating |

Logic bit `n` is digital input `D<n>` of the PPK2. A bit set in `logic_any`
but not in `logic_all` toggled inside the window.

The file may end with a partial record while it is still being written;
readers keep the remainder and wait for the rest.

## Reading it elsewhere

```python
import numpy as np

with open("run.ppk2", "rb") as f:
    header = f.readline().decode()
    fold = int(dict(kv.split("=") for kv in header.split()[2:])["fold"])
    dtype = np.dtype([("min", "<f4"), ("max", "<f4"), ("mean", "<f4"),
                      ("any", "u1"), ("all", "u1"), ("dropped", "<u2")])
    rec = np.fromfile(f, dtype=dtype)

t = np.arange(len(rec)) * fold / 100_000          # seconds
charge_uah = rec["mean"].sum() * fold / 100_000 / 3600
```
