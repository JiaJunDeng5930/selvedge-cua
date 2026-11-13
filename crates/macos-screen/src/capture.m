#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <dispatch/dispatch.h>
#import <math.h>
#import <time.h>

typedef struct {
    double x, y, width, height;
} DesktopRect;

typedef struct {
    DesktopRect desktop;
    uint32_t width, height;
    uint64_t callback_age_ns;
    uint8_t *pixels;
} ScreenFrame;

typedef enum {
    TargetMainDisplay,
    TargetDisplay,
    TargetWindow,
    TargetRegion,
} TargetKind;

static bool wait_for_capture(dispatch_semaphore_t semaphore) {
    return dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) == 0;
}

static bool fail(char *error, size_t capacity, NSString *message) {
    snprintf(error, capacity, "%s", message.UTF8String);
    return false;
}

static bool copy_pixels(CGImageRef image, uint64_t callback_time, CGRect desktop,
                        ScreenFrame *output, char *error, size_t capacity) {
    size_t width = CGImageGetWidth(image);
    size_t height = CGImageGetHeight(image);
    // A region spanning displays may be returned at their highest density. Preserve
    // the desktop extent and lower the image scale before allocating the RGBA buffer.
    const double longest_edge = 2560;
    double scale = fmin(1.0, longest_edge / fmax(width, height));
    width = fmax(1, round(width * scale));
    height = fmax(1, round(height * scale));
    uint8_t *pixels = calloc(height, width * 4);
    if (pixels == NULL) {
        // DEFENSE: A large screen frame can exhaust memory when captures overlap.
        return fail(error, capacity, @"Could not allocate screen frame pixels");
    }
    CGColorSpaceRef color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(pixels, width, height, 8, width * 4,
                                                 color_space, kCGImageAlphaPremultipliedLast |
                                                 kCGBitmapByteOrder32Big);
    CGColorSpaceRelease(color_space);
    if (context == NULL) {
        // DEFENSE: CoreGraphics can fail to allocate the bitmap backing context.
        free(pixels);
        return fail(error, capacity, @"Could not create screen frame bitmap");
    }
    CGContextDrawImage(context, CGRectMake(0, 0, width, height), image);
    CGContextRelease(context);
    // Quartz emits premultiplied RGBA. PNG uses straight alpha, including
    // rounded corners in independent window captures.
    for (size_t index = 0; index < width * height * 4; index += 4) {
        uint8_t alpha = pixels[index + 3];
        if (alpha > 0 && alpha < 255) {
            for (size_t channel = 0; channel < 3; channel++) {
                pixels[index + channel] = (pixels[index + channel] * 255u + alpha / 2u) / alpha;
            }
        }
    }
    *output = (ScreenFrame){
        {desktop.origin.x, desktop.origin.y, desktop.size.width, desktop.size.height},
        (uint32_t)width, (uint32_t)height,
        clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - callback_time, pixels
    };
    return true;
}

static bool capture_macos14(TargetKind kind, uint32_t identifier, CGRect region,
                            ScreenFrame *output, char *error, size_t capacity) API_AVAILABLE(macos(14.0));

static bool capture_macos14(TargetKind kind, uint32_t identifier, CGRect region,
                            ScreenFrame *output, char *error, size_t capacity) {
    __block id captured_image;
    __block NSError *capture_error;
    __block uint64_t callback_time;
    dispatch_semaphore_t image_ready = dispatch_semaphore_create(0);
    void (^record_image)(CGImageRef, NSError *) = ^(CGImageRef image, NSError *failure) {
        callback_time = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
        captured_image = image == NULL ? nil : CFBridgingRelease(CGImageRetain(image));
        capture_error = failure;
        dispatch_semaphore_signal(image_ready);
    };

    CGRect desktop = CGRectNull;
    if (@available(macOS 15.2, *)) {
        if (kind == TargetRegion) {
            desktop = region;
            [SCScreenshotManager captureImageInRect:region completionHandler:record_image];
        }
    }

    if (CGRectIsNull(desktop)) {
        // The desktop-independent filter uses the WindowServer connection
        // on the calling thread. A window-first script has not enumerated
        // windows or displays through Core Graphics yet.
        if (kind == TargetWindow) {
            CFArrayRef descriptions = CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, identifier);
            if (descriptions != NULL) {
                CFRelease(descriptions);
            }
        }
        dispatch_semaphore_t content_ready = dispatch_semaphore_create(0);
        __block SCShareableContent *content;
        __block NSError *content_error;
        [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                                   onScreenWindowsOnly:YES
                                                      completionHandler:^(SCShareableContent *shareable, NSError *failure) {
            content = shareable;
            content_error = failure;
            dispatch_semaphore_signal(content_ready);
        }];
        if (!wait_for_capture(content_ready)) {
            // DEFENSE: ScreenCaptureKit may fail to call back after a display or session disconnects.
            return fail(error, capacity, @"Timed out finding screens and windows");
        }
        if (content_error != nil) {
            return fail(error, capacity, content_error.localizedDescription);
        }

        SCContentFilter *filter;
        SCStreamConfiguration *config = [SCStreamConfiguration new];
        config.showsCursor = NO;
        if (kind == TargetWindow) {
            for (SCWindow *window in content.windows) {
                if (window.windowID == identifier) {
                    filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:window];
                    desktop = window.frame;
                    config.ignoreShadowsSingleWindow = YES;
                    config.ignoreGlobalClipSingleWindow = YES;
                    if (@available(macOS 14.2, *)) {
                        config.includeChildWindows = NO;
                    }
                    break;
                }
            }
            if (filter == nil) {
                return fail(error, capacity, @"The requested on-screen window was not found");
            }
        } else {
            CGDirectDisplayID display_id = kind == TargetDisplay ? identifier : CGMainDisplayID();
            for (SCDisplay *display in content.displays) {
                CGRect display_bounds = CGDisplayBounds(display.displayID);
                BOOL matches = kind == TargetRegion ? CGRectContainsRect(display_bounds, region)
                                                     : display.displayID == display_id;
                if (matches) {
                    filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
                    desktop = display_bounds;
                    if (kind == TargetRegion) {
                        desktop = region;
                        config.sourceRect = CGRectOffset(region, -display_bounds.origin.x, -display_bounds.origin.y);
                    }
                    break;
                }
            }
            if (filter == nil) {
                if (kind == TargetRegion) {
                    // KNOWN LIMITATION: ScreenCaptureKit before macOS 15.2 captures a region within one display.
                    return fail(error, capacity, @"The requested region must fit inside an active display on macOS before 15.2");
                }
                return fail(error, capacity, @"The requested display was not found");
            }
        }

        const double longest_edge = 2560;
        double scale = fmin(filter.pointPixelScale, longest_edge / fmax(desktop.size.width, desktop.size.height));
        config.width = fmax(1, round(desktop.size.width * scale));
        config.height = fmax(1, round(desktop.size.height * scale));
        [SCScreenshotManager captureImageWithFilter:filter configuration:config completionHandler:record_image];
    }

    if (!wait_for_capture(image_ready)) {
        // DEFENSE: A display or target window can disappear while a capture is in flight.
        return fail(error, capacity, @"Timed out capturing the screen");
    }
    if (captured_image == nil) {
        return fail(error, capacity, capture_error != nil ? capture_error.localizedDescription : @"No screen frame was returned");
    }
    return copy_pixels((__bridge CGImageRef)captured_image, callback_time, desktop, output, error, capacity);
}

bool selvedge_capture_screen(uint32_t kind, uint32_t identifier, DesktopRect region,
                             ScreenFrame *output, char *error, size_t capacity) {
    @autoreleasepool {
        if (@available(macOS 14.0, *)) {
            if (!CGPreflightScreenCaptureAccess()) {
                return fail(error, capacity, @"Screen Recording permission is required to observe the screen");
            }
            return capture_macos14(kind, identifier, CGRectMake(region.x, region.y, region.width, region.height),
                                   output, error, capacity);
        }
        return fail(error, capacity, @"Screen observation requires macOS 14 or later");
    }
}

void selvedge_free_screen_pixels(uint8_t *pixels) {
    free(pixels);
}

void selvedge_pump_main_run_loop(void) {
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.025, false);
}
