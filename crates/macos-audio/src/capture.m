#import <AVFoundation/AVFoundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <Foundation/Foundation.h>
#include <stdatomic.h>

// The IOProc is the sole writer. Readers copy only the published prefix.
typedef struct {
    AudioObjectID tap;
    AudioObjectID device;
    AudioDeviceIOProcID io_proc;
    bool started;
    uint32_t sample_rate;
    size_t capacity;
    _Atomic size_t written;
    _Atomic bool invalid;
    _Atomic bool stopped;
    float *samples;
    CFTypeRef owner;
} SelvedgeAudioCapture;

// The callback keeps its storage alive even if HAL teardown fails.
@interface SelvedgeAudioBufferOwner : NSObject {
@public
    SelvedgeAudioCapture *capture;
}
@end

@implementation SelvedgeAudioBufferOwner
- (void)dealloc {
    free(capture->samples);
    free(capture);
}
@end

static void capture_audio(SelvedgeAudioCapture *capture, const AudioBufferList *input) {
    if (atomic_load_explicit(&capture->stopped, memory_order_acquire) ||
        !input || input->mNumberBuffers == 0 || input->mBuffers[0].mDataByteSize == 0) {
        return;
    }
    const AudioBuffer *buffer = &input->mBuffers[0];
    if (input->mNumberBuffers != 1 || buffer->mNumberChannels != 1 ||
        buffer->mDataByteSize % sizeof(float) != 0 || !buffer->mData) {
        atomic_store_explicit(&capture->invalid, true, memory_order_release);
        return;
    }
    size_t written = atomic_load_explicit(&capture->written, memory_order_relaxed);
    size_t frames = MIN(capture->capacity - written, buffer->mDataByteSize / sizeof(float));
    memcpy(capture->samples + written, buffer->mData, frames * sizeof(float));
    atomic_store_explicit(&capture->written, written + frames, memory_order_release);
}

static OSStatus register_audio_callback(SelvedgeAudioBufferOwner *owner) {
    SelvedgeAudioCapture *capture = owner->capture;
    return AudioDeviceCreateIOProcIDWithBlock(
        &capture->io_proc, capture->device, NULL,
        ^(const AudioTimeStamp *now __unused, const AudioBufferList *input,
          const AudioTimeStamp *input_time __unused, AudioBufferList *output __unused,
          const AudioTimeStamp *output_time __unused) {
            capture_audio(owner->capture, input);
        });
}

OSStatus selvedge_audio_stop(SelvedgeAudioCapture *capture) {
    atomic_store_explicit(&capture->stopped, true, memory_order_release);
    if (!capture->started) {
        return noErr;
    }
    OSStatus status = AudioDeviceStop(capture->device, capture->io_proc);
    if (status == noErr) {
        capture->started = false;
    }
    return status;
}

void selvedge_audio_close(SelvedgeAudioCapture *capture) {
    selvedge_audio_stop(capture);
    if (capture->io_proc) {
        AudioDeviceDestroyIOProcID(capture->device, capture->io_proc);
    }
    if (capture->device != kAudioObjectUnknown) {
        AudioHardwareDestroyAggregateDevice(capture->device);
    }
    if (@available(macOS 14.2, *)) {
        if (capture->tap != kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(capture->tap);
        }
    }
    if (capture->owner) {
        CFRelease(capture->owner);
    } else {
        free(capture->samples);
        free(capture);
    }
}

SelvedgeAudioCapture *selvedge_audio_open(
    pid_t pid, uint32_t duration_ms, uint32_t *sample_rate, char *error, size_t error_size
) {
    @autoreleasepool {
        if (@available(macOS 14.2, *)) {
            AudioObjectPropertyAddress property = {
                kAudioHardwarePropertyTranslatePIDToProcessObject,
                kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain,
            };
            AudioObjectID process = kAudioObjectUnknown;
            UInt32 size = sizeof(process);
            OSStatus status = AudioObjectGetPropertyData(
                kAudioObjectSystemObject, &property, sizeof(pid), &pid, &size, &process
            );
            if (status != noErr || process == kAudioObjectUnknown) {
                snprintf(error, error_size, "Core Audio has no playback process for PID %d", pid);
                return NULL;
            }

            SelvedgeAudioCapture *capture = calloc(1, sizeof(*capture));
            if (!capture) {
                snprintf(error, error_size, "Could not allocate audio capture state");
                return NULL;
            }
            atomic_init(&capture->written, 0);
            atomic_init(&capture->invalid, false);
            atomic_init(&capture->stopped, false);
            SelvedgeAudioBufferOwner *owner = [SelvedgeAudioBufferOwner new];
            owner->capture = capture;
            capture->owner = CFBridgingRetain(owner);
            CATapDescription *description =
                [[CATapDescription alloc] initMonoMixdownOfProcesses:@[@(process)]];
            description.privateTap = YES;
            description.muteBehavior = CATapUnmuted;
            const char *step = "create the process tap";
            status = AudioHardwareCreateProcessTap(description, &capture->tap);
            if (status != noErr) {
                goto fail;
            }

            step = "get the tap PCM format";
            AudioStreamBasicDescription format = {0};
            property.mSelector = kAudioTapPropertyFormat;
            size = sizeof(format);
            status = AudioObjectGetPropertyData(capture->tap, &property, 0, NULL, &size, &format);
            if (status != noErr) {
                goto fail;
            }
            if (format.mFormatID != kAudioFormatLinearPCM ||
                (format.mFormatFlags & (kAudioFormatFlagIsFloat | kAudioFormatFlagIsBigEndian)) !=
                    kAudioFormatFlagIsFloat ||
                format.mChannelsPerFrame != 1 || format.mBytesPerFrame != sizeof(float) ||
                !(format.mSampleRate >= 8000 && format.mSampleRate <= 192000) ||
                format.mSampleRate != (uint32_t)format.mSampleRate) {
                snprintf(error, error_size, "Core Audio tap returned an unsupported PCM format");
                selvedge_audio_close(capture);
                return NULL;
            }
            capture->sample_rate = (uint32_t)format.mSampleRate;
            capture->capacity = ((size_t)capture->sample_rate * duration_ms + 999) / 1000;
            capture->samples = calloc(capture->capacity, sizeof(float));
            if (!capture->samples) {
                snprintf(error, error_size, "Could not allocate audio capture samples");
                selvedge_audio_close(capture);
                return NULL;
            }

            step = "create a private aggregate device";
            status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)@{
                @kAudioAggregateDeviceNameKey: @"Selvedge Audio Observation",
                @kAudioAggregateDeviceUIDKey: NSUUID.UUID.UUIDString,
                @kAudioAggregateDeviceIsPrivateKey: @YES,
                @kAudioAggregateDeviceTapListKey: @[@{
                    @kAudioSubTapUIDKey: description.UUID.UUIDString,
                    @kAudioSubTapDriftCompensationKey: @YES,
                }],
            }, &capture->device);
            if (status != noErr) {
                goto fail;
            }
            step = "register the audio capture callback";
            status = register_audio_callback(owner);
            if (status != noErr) {
                goto fail;
            }
            step = "start system audio capture (check system audio recording permission)";
            status = AudioDeviceStart(capture->device, capture->io_proc);
            if (status != noErr) {
                goto fail;
            }
            capture->started = true;
            *sample_rate = capture->sample_rate;
            return capture;

        fail:
            snprintf(error, error_size, "Core Audio could not %s (OSStatus %d)", step, status);
            selvedge_audio_close(capture);
            return NULL;
        }
        snprintf(error, error_size, "App audio capture requires macOS 14.2 or later");
        return NULL;
    }
}

// A negative result means the delivery format changed during capture.
ptrdiff_t selvedge_audio_read(
    const SelvedgeAudioCapture *capture, size_t offset, float *output, size_t capacity
) {
    if (atomic_load_explicit(&capture->invalid, memory_order_acquire)) {
        return -1;
    }
    size_t written = atomic_load_explicit(&capture->written, memory_order_acquire);
    size_t count = MIN(written > offset ? written - offset : 0, capacity);
    if (count != 0) {
        memcpy(output, capture->samples + offset, count * sizeof(float));
    }
    return (ptrdiff_t)count;
}

uint32_t selvedge_audio_resample(
    const float *samples, uint32_t length, uint32_t sample_rate, float *out, uint32_t capacity
) {
    @autoreleasepool {
        AVAudioFormat *input_format = [[AVAudioFormat alloc]
            initStandardFormatWithSampleRate:sample_rate channels:1];
        AVAudioFormat *output_format = [[AVAudioFormat alloc]
            initStandardFormatWithSampleRate:16000 channels:1];
        AVAudioPCMBuffer *input = [[AVAudioPCMBuffer alloc]
            initWithPCMFormat:input_format frameCapacity:length];
        AVAudioPCMBuffer *output = [[AVAudioPCMBuffer alloc]
            initWithPCMFormat:output_format frameCapacity:capacity];
        AVAudioConverter *converter = [[AVAudioConverter alloc]
            initFromFormat:input_format toFormat:output_format];
        if (!input || !output || !converter) {
            return 0;
        }
        input.frameLength = length;
        memcpy(input.floatChannelData[0], samples, length * sizeof(float));
        __block BOOL sent = NO;
        AVAudioConverterOutputStatus status = [converter
            convertToBuffer:output error:NULL
            withInputFromBlock:^AVAudioBuffer *(AVAudioPacketCount packets __unused,
                                               AVAudioConverterInputStatus *input_status) {
                *input_status = sent ? AVAudioConverterInputStatus_EndOfStream
                                     : AVAudioConverterInputStatus_HaveData;
                if (sent) {
                    return nil;
                }
                sent = YES;
                return input;
            }];
        if (status == AVAudioConverterOutputStatus_Error) {
            return 0;
        }
        memcpy(out, output.floatChannelData[0], output.frameLength * sizeof(float));
        return output.frameLength;
    }
}
