#include <stdio.h>

#define LIMIT 10
#define STEP(x) ((x) + 1)

struct lexer {
    int pos;
};

enum token {
    WORD,
    SPACE,
};

typedef int (*step_fn)(int);

static int *helper(int a, int b);

/* Advance to the limit. */
static int *helper(int a, int b)
{
    static int r;
    r = a;
    for (int i = 0; i < a; i++) {
        if (i > b) {
            r += STEP(i);
        } else if (i == b) {
            r -= i;
        }
    }
    while (r > LIMIT) {
        r--;
    }
    switch (r) {
    case 0:
        r = 1;
        break;
    default:
        r = 0;
        break;
    }
    return &r;
}

int main(void)
{
    int *p = helper(1, 2);
    printf("%d\n", *p);
    return p == 0 ? 1 : 0;
}
