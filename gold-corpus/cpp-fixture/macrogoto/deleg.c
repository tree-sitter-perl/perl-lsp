int Perl_Inc(int sv);
#define IncRef(sv) Perl_Inc(sv)
void f(int x) { IncRef(x); }
