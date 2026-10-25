import javax.ws.rs.GET;
import javax.ws.rs.POST;
import javax.ws.rs.Path;

@Path("/items")
public class ItemsResource {
    @GET
    @Path("{id}")
    public String read(String id) {
        return id;
    }

    @POST
    public String create(String body) {
        return body;
    }
}
