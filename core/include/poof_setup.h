// Mac setup app interface to poof-core (built with the "desktop" feature).
// Every call returns immediately except poof_usb_devices_json (≤2 s).

#ifndef POOF_SETUP_H
#define POOF_SETUP_H

#include <stdbool.h>
#include <stdint.h>

#pragma clang assume_nonnull begin

#define POOF_OK 0
#define POOF_ERR_ARG -1
#define POOF_ERR_RUNTIME -3
#define POOF_ERR_BUSY -4

// Pairs the first USB iPhone, enables network lockdown, and copies the pairing
// file into Poof. With apple_id non-NULL it first signs and installs ipa_path.
// anisette_url: NULL for the default anisette server.
int32_t poof_setup_start(const char *_Nullable apple_id, const char *_Nullable password,
                         const char *_Nullable ipa_path, const char *pairing_out,
                         bool revoke_certs, const char *_Nullable anisette_url);

// Answers a pending two-factor prompt: the code, or "device", "sms",
// "sms:<id>", "resend". NULL or "" aborts.
int32_t poof_setup_submit_code(const char *_Nullable code);

// Stops a running setup.
int32_t poof_setup_cancel(void);

// {"running":bool,"log":[...],"needs_code":bool,
//  "code_prompt":{"last_error":str|null,"sms":bool,"unknown":bool,"numbers":[{"id":7,"last_two":"42"}]}|null,
//  "report":{...}|null,"error":str|null}
char *_Nullable poof_setup_status_json(void);

// ["udid", ...]
char *_Nullable poof_usb_devices_json(void);

void poof_string_free(char *_Nullable s);

#pragma clang assume_nonnull end

#endif
