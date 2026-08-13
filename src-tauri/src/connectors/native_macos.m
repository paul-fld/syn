#import <Foundation/Foundation.h>
#import <Contacts/Contacts.h>
#import <EventKit/EventKit.h>
#import <Photos/Photos.h>
#import <ApplicationServices/ApplicationServices.h>
#include <stdlib.h>
#include <string.h>

static char *syn_copy_json(id object) {
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:object options:0 error:&error];
    if (!data || error) return NULL;
    NSString *string = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return string ? strdup(string.UTF8String) : NULL;
}

void syn_native_free(char *value) { if (value) free(value); }

// 0 inconnu, 1 autorisé, 2 refusé, 3 restreint, 4 limité, -1 indisponible.
int32_t syn_native_permission_status(const char *raw_service) {
    NSString *service = [NSString stringWithUTF8String:raw_service ?: ""];
    if ([service isEqualToString:@"contacts"]) {
        CNAuthorizationStatus s = [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts];
        return s == CNAuthorizationStatusAuthorized ? 1 : s == CNAuthorizationStatusDenied ? 2 : s == CNAuthorizationStatusRestricted ? 3 : 0;
    }
    if ([service isEqualToString:@"calendar"] || [service isEqualToString:@"reminders"]) {
        EKEntityType type = [service isEqualToString:@"calendar"] ? EKEntityTypeEvent : EKEntityTypeReminder;
        EKAuthorizationStatus s = [EKEventStore authorizationStatusForEntityType:type];
        if (@available(macOS 14.0, *)) {
            if (s == EKAuthorizationStatusFullAccess) return 1;
        } else if (s == EKAuthorizationStatusAuthorized) return 1;
        return s == EKAuthorizationStatusDenied ? 2 : s == EKAuthorizationStatusRestricted ? 3 : 0;
    }
    if ([service isEqualToString:@"photos"]) {
        PHAuthorizationStatus s = [PHPhotoLibrary authorizationStatusForAccessLevel:PHAccessLevelReadWrite];
        return s == PHAuthorizationStatusAuthorized ? 1 : s == PHAuthorizationStatusLimited ? 4 : s == PHAuthorizationStatusDenied ? 2 : s == PHAuthorizationStatusRestricted ? 3 : 0;
    }
    if ([service isEqualToString:@"screen"]) return AXIsProcessTrusted() ? 1 : 0;
    return -1;
}

int32_t syn_native_request_permission(const char *raw_service) {
    NSString *service = [NSString stringWithUTF8String:raw_service ?: ""];
    if ([service isEqualToString:@"screen"]) {
        NSDictionary *options = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES};
        return AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options) ? 1 : 0;
    }
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block BOOL granted = NO;
    if ([service isEqualToString:@"contacts"]) {
        CNContactStore *store = [[CNContactStore alloc] init];
        [store requestAccessForEntityType:CNEntityTypeContacts completionHandler:^(BOOL ok, NSError *error) {
            granted = ok && error == nil;
            dispatch_semaphore_signal(semaphore);
        }];
    } else if ([service isEqualToString:@"calendar"] || [service isEqualToString:@"reminders"]) {
        EKEventStore *store = [[EKEventStore alloc] init];
        void (^completion)(BOOL, NSError *) = ^(BOOL ok, NSError *error) {
            granted = ok && error == nil;
            dispatch_semaphore_signal(semaphore);
        };
        if (@available(macOS 14.0, *)) {
            if ([service isEqualToString:@"calendar"]) [store requestFullAccessToEventsWithCompletion:completion];
            else [store requestFullAccessToRemindersWithCompletion:completion];
        } else {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
            [store requestAccessToEntityType:[service isEqualToString:@"calendar"] ? EKEntityTypeEvent : EKEntityTypeReminder completion:completion];
#pragma clang diagnostic pop
        }
    } else if ([service isEqualToString:@"photos"]) {
        [PHPhotoLibrary requestAuthorizationForAccessLevel:PHAccessLevelReadWrite handler:^(PHAuthorizationStatus status) {
            granted = status == PHAuthorizationStatusAuthorized || status == PHAuthorizationStatusLimited;
            dispatch_semaphore_signal(semaphore);
        }];
    } else {
        return -1;
    }
    dispatch_semaphore_wait(semaphore, DISPATCH_TIME_FOREVER);
    return granted ? 1 : syn_native_permission_status(raw_service);
}

char *syn_native_contacts_json(void) {
    if (syn_native_permission_status("contacts") != 1) return NULL;
    CNContactStore *store = [[CNContactStore alloc] init];
    NSArray *keys = @[CNContactGivenNameKey, CNContactFamilyNameKey, CNContactEmailAddressesKey, CNContactPhoneNumbersKey];
    CNContactFetchRequest *request = [[CNContactFetchRequest alloc] initWithKeysToFetch:keys];
    NSMutableArray *contacts = [NSMutableArray array];
    NSError *error = nil;
    [store enumerateContactsWithFetchRequest:request error:&error usingBlock:^(CNContact *contact, BOOL *stop) {
        NSString *name = [[NSString stringWithFormat:@"%@ %@", contact.givenName ?: @"", contact.familyName ?: @""] stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet];
        if (name.length == 0) return;
        NSString *email = contact.emailAddresses.firstObject.value ?: @"";
        NSString *phone = contact.phoneNumbers.firstObject.value.stringValue ?: @"";
        [contacts addObject:@{@"name": name, @"email": email, @"phone": phone}];
    }];
    return error ? NULL : syn_copy_json(contacts);
}

char *syn_native_calendar_events(double from, double to) {
    if (syn_native_permission_status("calendar") != 1) return NULL;
    EKEventStore *store = [[EKEventStore alloc] init];
    NSPredicate *predicate = [store predicateForEventsWithStartDate:[NSDate dateWithTimeIntervalSince1970:from]
                                                              endDate:[NSDate dateWithTimeIntervalSince1970:to]
                                                            calendars:nil];
    NSMutableArray *events = [NSMutableArray array];
    for (EKEvent *event in [store eventsMatchingPredicate:predicate]) {
        [events addObject:@{
            @"id": event.eventIdentifier ?: @"",
            @"title": event.title ?: @"",
            @"start": @([event.startDate timeIntervalSince1970]),
            @"end": @([event.endDate timeIntervalSince1970]),
            @"location": event.location ?: [NSNull null],
            @"notes": event.notes ?: [NSNull null],
            @"source": @"apple"
        }];
    }
    return syn_copy_json(events);
}

char *syn_native_calendar_create(const char *raw_title, double start, double end, const char *raw_location) {
    if (syn_native_permission_status("calendar") != 1) return NULL;
    EKEventStore *store = [[EKEventStore alloc] init];
    EKEvent *event = [EKEvent eventWithEventStore:store];
    event.title = [NSString stringWithUTF8String:raw_title ?: ""];
    event.startDate = [NSDate dateWithTimeIntervalSince1970:start];
    event.endDate = [NSDate dateWithTimeIntervalSince1970:end > start ? end : start + 3600.0];
    event.location = [NSString stringWithUTF8String:raw_location ?: ""];
    event.calendar = store.defaultCalendarForNewEvents;
    NSError *error = nil;
    if (![store saveEvent:event span:EKSpanThisEvent commit:YES error:&error] || error) return NULL;
    return syn_copy_json(@{@"id": event.eventIdentifier ?: @"", @"title": event.title ?: @"", @"start": @(start), @"end": @(end > start ? end : start + 3600.0), @"source": @"apple"});
}

int32_t syn_native_calendar_delete(const char *raw_identifier) {
    if (syn_native_permission_status("calendar") != 1) return 0;
    EKEventStore *store = [[EKEventStore alloc] init];
    NSString *identifier = [NSString stringWithUTF8String:raw_identifier ?: ""];
    EKEvent *event = [store eventWithIdentifier:identifier];
    if (!event) return 0;
    NSError *error = nil;
    return [store removeEvent:event span:EKSpanThisEvent commit:YES error:&error] && error == nil ? 1 : 0;
}
