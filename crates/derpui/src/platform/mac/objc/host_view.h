#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

@interface NSPasteboard (Derp)
- (NSArray *)getFilePaths;
@end

/// DerpHostView is the Content view of a Derp window.
// It is backed by a Metal CALayer.
@interface DerpHostView : NSView <CALayerDelegate, NSTextInputClient>
- (DerpHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)metalDevice
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testMode;
- (void)setAsyncCallback:(BOOL)shouldAsync;
- (void)setPresentsWithTransaction:(BOOL)presentsWithTransaction;
- (BOOL)keyDownImpl:(NSEvent *)event;
@end
