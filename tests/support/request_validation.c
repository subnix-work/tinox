#define main tinox_runtime_main
#include "../../runtime/runtime.c"
#undef main
#include <assert.h>
int main(void) {
 const char *defs="{\"Input\":[{\"name\":\"text\",\"schema\":{\"type\":\"string\",\"required\":true}},{\"name\":\"values\",\"schema\":{\"type\":\"array\",\"items\":{\"type\":\"integer\",\"bits\":8,\"unsigned\":true,\"required\":true}}}]}";
 const char *valid="{\"text\":\"abc\\u00e9\\ud83e\\udd80\",\"values\":[0,255]}";
 for(size_t i=0;i<strlen(valid);i++){char *prefix=strndup(valid,i);int64_t *parsed=NULL;assert(tinox_request_validate(prefix,defs,"Input",&parsed));free(prefix);}
 const char *invalid[]={"{\"text\":\"abc\\", "{\"text\":\"\\u", "{\"text\":\"\\ud800\\u", "{\"text\":\"\\udfff\"}", "{\"text\":\"\\u0000\"}", "{\"text\":\"\xc0\x80\"}", "{\"text\":\"ok\",\"values\":[256]}", "{\"text\":\"ok\",\"values\":[-1]}", "\v{}", "\f{}"};
 for(size_t i=0;i<sizeof(invalid)/sizeof(invalid[0]);i++){int64_t *parsed=NULL;assert(tinox_request_validate(invalid[i],defs,"Input",&parsed));}
 int64_t *parsed=NULL;assert(!tinox_request_validate(valid,defs,"Input",&parsed));
 char *huge=malloc(1048578);memset(huge,' ',1048577);huge[1048577]=0;assert(tinox_request_validate(huge,defs,"Input",&parsed));free(huge);
 puts("strict DTO parser ASan checks passed");
}
