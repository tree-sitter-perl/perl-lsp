struct Point { int x; int y; };
int main() {
  struct Point p {1, 2};
  return p.x;
}
