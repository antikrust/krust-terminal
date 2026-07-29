#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

@interface NSPasteboard (Bork)
- (NSArray *)getFilePaths;
@end

/// BorkHostView is the Content view of a Bork window.
// It is backed by a Metal CALayer.
@interface BorkHostView : NSView <CALayerDelegate, NSTextInputClient>
- (BorkHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)metalDevice
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testMode;
- (void)setAsyncCallback:(BOOL)shouldAsync;
- (void)setPresentsWithTransaction:(BOOL)presentsWithTransaction;
- (BOOL)keyDownImpl:(NSEvent *)event;
@end
