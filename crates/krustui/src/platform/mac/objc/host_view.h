#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

@interface NSPasteboard (Krust)
- (NSArray *)getFilePaths;
@end

/// KrustHostView is the Content view of a Krust window.
// It is backed by a Metal CALayer.
@interface KrustHostView : NSView <CALayerDelegate, NSTextInputClient>
- (KrustHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)metalDevice
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testMode;
- (void)setAsyncCallback:(BOOL)shouldAsync;
- (void)setPresentsWithTransaction:(BOOL)presentsWithTransaction;
- (BOOL)keyDownImpl:(NSEvent *)event;
@end
