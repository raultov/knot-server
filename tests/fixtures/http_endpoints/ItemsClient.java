/** OpenFeign client of ItemsResource: linked to GET /items/{id} by HTTP_CALLS (knot 1.14). */
@FeignClient(name = "items")
public interface ItemsClient {
    @GetMapping("/items/{id}")
    String fetch(@PathVariable("id") String id);
}
