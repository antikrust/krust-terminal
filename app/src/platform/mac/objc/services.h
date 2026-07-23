// Our class for handling NSServices messages.
@interface DerpServicesProvider : NSObject
@end

// Functions implemented in Rust.
id derp_services_provider_custom_url_scheme();
void derp_app_open_urls(id app, id urls);
