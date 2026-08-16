#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <Contacts/Contacts.h>
#import <EventKit/EventKit.h>
#import <Photos/Photos.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Vision/Vision.h>
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
    if ([service isEqualToString:@"screen"]) {
        if (@available(macOS 10.15, *)) return CGPreflightScreenCaptureAccess() ? 1 : 0;
        return 1;
    }
    return -1;
}

int32_t syn_native_request_permission(const char *raw_service) {
    NSString *service = [NSString stringWithUTF8String:raw_service ?: ""];
    if ([service isEqualToString:@"screen"]) {
        if (@available(macOS 10.15, *)) return CGRequestScreenCaptureAccess() ? 1 : 0;
        return 1;
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

char *syn_native_ocr_image_json(const char *raw_path) {
    NSString *path = [NSString stringWithUTF8String:raw_path ?: ""];
    NSURL *url = [NSURL fileURLWithPath:path];
    VNRecognizeTextRequest *request = [[VNRecognizeTextRequest alloc] init];
    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
    request.usesLanguageCorrection = YES;
    request.recognitionLanguages = @[@"fr-FR", @"en-US"];
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithURL:url options:@{}];
    NSError *error = nil;
    if (![handler performRequests:@[request] error:&error] || error) return NULL;

    NSMutableArray *items = [NSMutableArray array];
    NSUInteger limit = MIN(request.results.count, (NSUInteger)300);
    for (NSUInteger i = 0; i < limit; i++) {
        VNRecognizedTextObservation *observation = request.results[i];
        VNRecognizedText *candidate = [observation topCandidates:1].firstObject;
        if (!candidate || candidate.string.length == 0) continue;
        CGRect box = observation.boundingBox;
        [items addObject:@{
            @"text": candidate.string,
            @"confidence": @(candidate.confidence),
            @"x": @(box.origin.x), @"y": @(box.origin.y),
            @"width": @(box.size.width), @"height": @(box.size.height)
        }];
    }
    return syn_copy_json(items);
}

char *syn_native_frontmost_context_json(void) {
    pid_t ownPID = NSProcessInfo.processInfo.processIdentifier;
    NSString *app = @"";
    NSString *title = @"";
    pid_t targetPID = 0;
    CFArrayRef rawWindows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID
    );
    NSArray *windows = CFBridgingRelease(rawWindows);
    // CGWindowList est ordonnée de l'avant vers l'arrière. On ignore toutes les
    // fenêtres du processus Syn, car masquer une fenêtre ne change pas toujours
    // immédiatement NSWorkspace.frontmostApplication.
    for (NSDictionary *window in windows) {
        pid_t pid = [window[(id)kCGWindowOwnerPID] intValue];
        if (pid == ownPID) continue;
        if ([window[(id)kCGWindowLayer] intValue] != 0) continue;
        if (![window[(id)kCGWindowIsOnscreen] boolValue]) continue;
        CGRect bounds = CGRectZero;
        CFDictionaryRef rawBounds = (__bridge CFDictionaryRef)window[(id)kCGWindowBounds];
        if (!rawBounds || !CGRectMakeWithDictionaryRepresentation(rawBounds, &bounds)) continue;
        if (bounds.size.width < 160 || bounds.size.height < 100) continue;
        NSString *owner = window[(id)kCGWindowOwnerName] ?: @"";
        if ([owner isEqualToString:@"Window Server"] || [owner isEqualToString:@"Dock"]) continue;
        targetPID = pid;
        NSRunningApplication *target = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
        app = target.localizedName.length > 0 ? target.localizedName : owner;
        NSString *candidate = window[(id)kCGWindowName];
        title = candidate.length > 0 ? candidate : @"";
        break;
    }
    if (targetPID == 0) {
        // Sans autre fenêtre applicative, ce qui est visible est le Bureau/Finder.
        app = @"Finder";
        title = @"Bureau macOS";
    }
    return syn_copy_json(@{
        @"available": @YES,
        @"app": app,
        @"window": title,
        @"target_pid": @(targetPID),
        @"selection": @"topmost_external_window"
    });
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

char *syn_native_reminders_json(void) {
    if (syn_native_permission_status("reminders") != 1) return NULL;
    EKEventStore *store = [[EKEventStore alloc] init];
    NSPredicate *predicate = [store predicateForIncompleteRemindersWithDueDateStarting:nil ending:nil calendars:nil];
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block NSArray<EKReminder *> *fetched = nil;
    [store fetchRemindersMatchingPredicate:predicate completion:^(NSArray<EKReminder *> *reminders) {
        fetched = reminders;
        dispatch_semaphore_signal(semaphore);
    }];
    if (dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) != 0) return NULL;
    NSMutableArray *items = [NSMutableArray array];
    for (EKReminder *reminder in fetched) {
        NSDate *due = reminder.dueDateComponents ? [NSCalendar.currentCalendar dateFromComponents:reminder.dueDateComponents] : nil;
        [items addObject:@{
            @"id": reminder.calendarItemIdentifier ?: @"",
            @"title": reminder.title ?: @"",
            @"due": due ? @([due timeIntervalSince1970]) : [NSNull null],
            @"list": reminder.calendar.title ?: @""
        }];
    }
    return syn_copy_json(items);
}

char *syn_native_reminder_create(const char *raw_title, double due) {
    if (syn_native_permission_status("reminders") != 1) return NULL;
    EKEventStore *store = [[EKEventStore alloc] init];
    EKReminder *reminder = [EKReminder reminderWithEventStore:store];
    reminder.title = [NSString stringWithUTF8String:raw_title ?: ""];
    reminder.calendar = store.defaultCalendarForNewReminders;
    if (due > 0) {
        NSDate *date = [NSDate dateWithTimeIntervalSince1970:due];
        reminder.dueDateComponents = [NSCalendar.currentCalendar
            components:(NSCalendarUnitYear | NSCalendarUnitMonth | NSCalendarUnitDay | NSCalendarUnitHour | NSCalendarUnitMinute)
              fromDate:date];
        [reminder addAlarm:[EKAlarm alarmWithAbsoluteDate:date]];
    }
    NSError *error = nil;
    if (![store saveReminder:reminder commit:YES error:&error] || error) return NULL;
    return syn_copy_json(@{@"id": reminder.calendarItemIdentifier ?: @"", @"title": reminder.title ?: @""});
}

int32_t syn_native_reminder_complete(const char *raw_identifier) {
    if (syn_native_permission_status("reminders") != 1) return 0;
    EKEventStore *store = [[EKEventStore alloc] init];
    NSString *identifier = [NSString stringWithUTF8String:raw_identifier ?: ""];
    EKCalendarItem *item = [store calendarItemWithIdentifier:identifier];
    if (![item isKindOfClass:[EKReminder class]]) return 0;
    EKReminder *reminder = (EKReminder *)item;
    reminder.completed = YES;
    NSError *error = nil;
    return [store saveReminder:reminder commit:YES error:&error] && error == nil ? 1 : 0;
}
