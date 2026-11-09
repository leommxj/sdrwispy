#import <CoreLocation/CoreLocation.h>
#import <CoreWLAN/CoreWLAN.h>
#import <Foundation/Foundation.h>
#include <stdlib.h>
#include <string.h>

@interface SDRWispyLocationDelegate : NSObject <CLLocationManagerDelegate>
@end
@implementation SDRWispyLocationDelegate
- (void)locationManagerDidChangeAuthorization:(CLLocationManager *)manager {
    // A subsequent scan reads the new authorization status.
}
@end

static NSString *location_status(BOOL requestPermission) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    CLAuthorizationStatus status = [CLLocationManager authorizationStatus];
#pragma clang diagnostic pop
    if (status == kCLAuthorizationStatusNotDetermined && requestPermission &&
        [[NSBundle mainBundle] objectForInfoDictionaryKey:@"NSLocationUsageDescription"]) {
        dispatch_async(dispatch_get_main_queue(), ^{
            static CLLocationManager *manager;
            static SDRWispyLocationDelegate *delegate;
            if (!manager) {
                delegate = [SDRWispyLocationDelegate new];
                manager = [CLLocationManager new];
                manager.delegate = delegate;
                [manager requestWhenInUseAuthorization];
            }
        });
    }
    switch (status) {
        case kCLAuthorizationStatusNotDetermined: return @"not_determined";
        case kCLAuthorizationStatusRestricted: return @"restricted";
        case kCLAuthorizationStatusDenied: return @"denied";
        case kCLAuthorizationStatusAuthorizedAlways: return @"authorized";
        default: return @"unknown";
    }
}

static NSArray<NSNumber *> *data_bytes(NSData *data) {
    NSMutableArray *result = [NSMutableArray arrayWithCapacity:data.length];
    const unsigned char *bytes = data.bytes;
    for (NSUInteger i = 0; i < data.length; i++) [result addObject:@(bytes[i])];
    return result;
}

static NSString *phy_label(CWNetwork *network) {
    if ([network supportsPHYMode:kCWPHYMode11ax]) return @"802.11ax";
    if ([network supportsPHYMode:kCWPHYMode11ac]) return @"802.11ac";
    if ([network supportsPHYMode:kCWPHYMode11n]) return @"802.11n";
    if ([network supportsPHYMode:kCWPHYMode11a]) return @"802.11a";
    if ([network supportsPHYMode:kCWPHYMode11g]) return @"802.11g";
    if ([network supportsPHYMode:kCWPHYMode11b]) return @"802.11b";
    return @"unknown";
}

static NSInteger width_mhz(CWChannelWidth width) {
    switch (width) {
        case kCWChannelWidth20MHz: return 20;
        case kCWChannelWidth40MHz: return 40;
        case kCWChannelWidth80MHz: return 80;
        case kCWChannelWidth160MHz: return 160;
        default: return 0;
    }
}

static NSDictionary *scan(const char *selectedInterface, BOOL requestPermission) {
    NSString *authorization = location_status(requestPermission);
    CWWiFiClient *client = [CWWiFiClient sharedWiFiClient];
    NSArray<NSString *> *interfaces;
    if (@available(macOS 13.0, *)) {
        interfaces = [client interfaceNames];
    } else {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        interfaces = [CWWiFiClient interfaceNames];
#pragma clang diagnostic pop
    }
    interfaces = [interfaces sortedArrayUsingSelector:@selector(compare:)];
    if (!interfaces) interfaces = @[];
    NSString *selected = selectedInterface ? [NSString stringWithUTF8String:selectedInterface] : @"";
    if (!selected.length) selected = interfaces.firstObject;
    NSMutableDictionary *report = [@{@"interfaces": interfaces,
                                     @"authorization": authorization,
                                     @"networks": @[]} mutableCopy];
    if (!selected) {
        report[@"error"] = @"No Wi-Fi interfaces are available.";
        return report;
    }
    if (![interfaces containsObject:selected]) {
        report[@"error"] = [NSString stringWithFormat:@"Selected interface '%@' is not available.", selected];
        return report;
    }
    report[@"interface"] = selected;
    CWInterface *interface = [client interfaceWithName:selected];
    if (!interface.powerOn) {
        report[@"error"] = [NSString stringWithFormat:@"Wi-Fi is powered off on %@.", selected];
        return report;
    }
    NSError *error = nil;
    NSSet<CWNetwork *> *networks = [interface scanForNetworksWithName:nil error:&error];
    if (!networks) {
        report[@"error"] = [NSString stringWithFormat:@"CoreWLAN scan failed (%@ %ld): %@",
            error.domain, (long)error.code, error.localizedDescription];
        return report;
    }
    NSMutableArray *rows = [NSMutableArray arrayWithCapacity:networks.count];
    for (CWNetwork *network in networks) {
        CWChannel *channel = network.wlanChannel;
        [rows addObject:@{@"ssid": data_bytes(network.ssidData),
                          @"bssid": network.bssid ?: @"",
                          @"channel": @(channel.channelNumber),
                          @"band": @(channel.channelBand),
                          @"width_mhz": @(width_mhz(channel.channelWidth)),
                          @"rssi_dbm": @(network.rssiValue),
                          @"phy": phy_label(network),
                          @"ies": data_bytes(network.informationElementData)}];
    }
    report[@"networks"] = rows;
    return report;
}

char *sdrwispy_wifi_scan(const char *selectedInterface, bool requestPermission) {
    @autoreleasepool {
        NSDictionary *report;
        @try {
            report = scan(selectedInterface, requestPermission);
        } @catch (NSException *exception) {
            report = @{@"interfaces": @[], @"networks": @[], @"authorization": @"unknown",
                       @"error": exception.reason ?: @"CoreWLAN raised an exception."};
        }
        NSData *json = [NSJSONSerialization dataWithJSONObject:report options:0 error:nil];
        return json ? strndup(json.bytes, json.length) : NULL;
    }
}

void sdrwispy_wifi_free(char *json) {
    free(json);
}
