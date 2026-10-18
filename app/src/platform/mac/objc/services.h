// Our class for handling NSServices messages.
@interface KrustServicesProvider : NSObject
@end

// Functions implemented in Rust.
id krust_services_provider_custom_url_scheme();
void krust_app_open_urls(id app, id urls);
