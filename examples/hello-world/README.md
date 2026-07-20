# hello-world

The smallest Nymph program: print a line and exit.

```nym
import std/io with (println)

func main() = {
  println("Hello, world!")
}
```

`main()` takes no arguments and returns nothing — it's the entry point the runner
calls. `println` comes from `std/io`; string arguments are printed as-is.

**Status:** ✅ Runs today.

```sh
nymph run
# Hello, world!
```
