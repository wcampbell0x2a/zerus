# Development

All patches and merge requests are welcome!

```console
$ just build              # build for x86_64-unknown-linux-musl, with cross
$ just test               # run all the tests
$ just demo               # serve a demo mirror at http://127.0.0.1:3000
$ just book               # build this book, with new screenshots
```

`just book-screenshots` takes the screenshots of the web UI with headless Firefox. The
images are not in git. The book workflow makes them again each time it builds the book.
