#define LOG emit_log_record_with_a_long_name(1, 2, 3)
void emit_log_record_with_a_long_name(int a, int b, int c);
struct Widget { int size; };
int main() {
  struct Widget w;
  LOG; w.size = 5;
  return w.size;
}
