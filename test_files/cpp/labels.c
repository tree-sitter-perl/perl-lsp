void f(int x) {
    if (x) goto done;
    x = x + 1;
done:
    return;
}
