#import <Foundation/Foundation.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

char *sdrwispy_wifi_scan(const char *interface, bool requestPermission);
void sdrwispy_wifi_free(char *json);

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        const char *interface = argc > 1 && strcmp(argv[1], "--json") != 0 ? argv[1] : "";
        bool full = argc > 1 && strcmp(argv[argc - 1], "--json") == 0;
        // Test the production scanner, without prompting or changing permissions.
        char *json = sdrwispy_wifi_scan(interface, false);
        if (!json) return 1;
        NSData *data = [NSData dataWithBytes:json length:strlen(json)];
        NSDictionary *report = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
        if (!report) { sdrwispy_wifi_free(json); return 1; }
        if (full) {
            puts(json);
        } else {
            NSMutableDictionary *summary = [report mutableCopy];
            NSMutableArray *networks = [NSMutableArray new];
            for (NSDictionary *network in report[@"networks"]) {
                NSMutableDictionary *row = [network mutableCopy];
                row[@"has_ssid"] = @([network[@"ssid"] count] > 0);
                row[@"has_bssid"] = @([network[@"bssid"] length] > 0);
                [row removeObjectsForKeys:@[@"ssid", @"bssid", @"ies"]];
                [networks addObject:row];
            }
            summary[@"networks"] = networks;
            NSData *output = [NSJSONSerialization dataWithJSONObject:summary options:NSJSONWritingPrettyPrinted error:nil];
            puts([[NSString alloc] initWithData:output encoding:NSUTF8StringEncoding].UTF8String);
        }
        sdrwispy_wifi_free(json);
        return report[@"error"] ? 1 : 0;
    }
}
