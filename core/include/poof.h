// C interface to poof-core. Every call returns immediately; the engine runs on
// its own threads. Poll poof_status_json() for progress and errors.

#ifndef POOF_H
#define POOF_H

#include <stdint.h>

#pragma clang assume_nonnull begin

#define POOF_OK 0
#define POOF_ERR_ARG -1
#define POOF_ERR_NOT_RUNNING -2
#define POOF_ERR_RUNTIME -3

// Starts or restarts the engine.
//   pairing_file: path to the device's pairing plist
//   device_ip:    "10.7.0.1" through an on-device loopback VPN
//   ddi_dir:      folder with Image.dmg, Image.dmg.trustcache, BuildManifest.plist,
//                 or NULL if the developer disk image is already mounted
//   refresh_secs: re-send interval for a static location; 0 disables
int32_t poof_start(const char *pairing_file, const char *device_ip,
                   const char *_Nullable ddi_dir, uint32_t refresh_secs);

int32_t poof_set_location(double lat, double lon);

// {"points":[{"lat":..,"lon":..},...],"speed_mps":1.4,"loop":false}
int32_t poof_walk(const char *route_json);

// Back to real GPS; stays connected.
int32_t poof_clear(void);

// Back to real GPS and disconnect.
int32_t poof_stop(void);

// Caller frees with poof_string_free.
char *_Nullable poof_status_json(void);
void poof_string_free(char *_Nullable s);

#pragma clang assume_nonnull end

#endif
