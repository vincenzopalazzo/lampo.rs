#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


/**
 * LampoDaemon is the main data structure that uses the facade
 * pattern to hide the complexity of the LDK library. You can interact
 * with the LampoDaemon's components through access
 * methods (similar to get methods in modern procedural languages).
 *
 * Another way to view the LampoDaemon is as
 * a microkernel pattern, especially for developers
 * who are interested in building their own node on
 * top of the LampoDaemon.
 */
typedef struct LampoDeamon LampoDeamon;

/**
 * Add a JSON RPC 2.0 Sever that listen on a unixsocket, and return a error code
 * < 0 is an error happens, or 0 is all goes well.
 */
int64_t add_jsonrpc_on_unixsocket(struct LampoDeamon *lampod);

/**
 * Allow to create a lampo deamon from a configuration patch!
 */
void free_lampod(struct LampoDeamon *lampod);

/**
 * Add a JSON RPC 2.0 Sever that listen on a unixsocket, and return a error code
 * < 0 is an error happens, or 0 is all goes well.
 */
const char *lampo_last_errror(void);

/**
 * Allow to create a lampo deamon from a configuration patch!
 */
void lampo_listen(struct LampoDeamon *lampod);

const char *lampod_call(struct LampoDeamon *lampod, const char *method, const char *buffer);

/**
 * Allow to create a lampo deamon from a configuration patch!
 */
struct LampoDeamon *new_lampod(const char *conf_path);
